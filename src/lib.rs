//! floe - a VM-backed harness for kernel tasks.
//!
//! The core is Rust because everything here is process and VM lifecycle:
//! spawning children that can hang, killing process groups, capturing serial
//! consoles, classifying faults. Python drives it.
//!
//! The trial spine, which is not Harbor's environment-as-sandbox model:
//!
//! ```text
//! workspace  (host git worktree; the agent edits here)
//!   -> build   vng -b       -> bzImage
//!   -> boot    vng -r -e    -> ephemeral guest, console captured
//!   -> grade                -> reward computed HOST-side from evidence
//! ```

pub mod grade;
pub mod proc;
pub mod task;
pub mod trial;
pub mod vmm;
pub mod vsock;
pub mod workspace;

pub use grade::Outcome;
pub use trial::{Agent, Config, TrialResult};

#[cfg(feature = "python")]
mod python {
    use std::path::PathBuf;

    use pyo3::exceptions::PyRuntimeError;
    use pyo3::prelude::*;
    use pyo3::types::PyDict;

    use crate::trial::{self, Agent, Config};

    /// Run one trial. Returns a dict so the Python side stays free to render
    /// it however it likes without tracking a Rust type.
    #[pyfunction]
    #[pyo3(signature = (task_dir, agent, kernel_src, runs_dir, ccache_dir, rootfs, keep=false))]
    fn run_trial<'py>(
        py: Python<'py>,
        task_dir: PathBuf,
        agent: &str,
        kernel_src: PathBuf,
        runs_dir: PathBuf,
        ccache_dir: PathBuf,
        rootfs: PathBuf,
        keep: bool,
    ) -> PyResult<Bound<'py, PyDict>> {
        let agent = Agent::parse(agent).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;
        let cfg = Config {
            kernel_src,
            runs_dir,
            ccache_dir,
            rootfs,
            keep,
        };

        // A trial spends minutes in build and boot. Releasing the GIL keeps a
        // Python caller free to run several concurrently.
        let result = py
            .allow_threads(|| trial::run_trial(&task_dir, agent, &cfg))
            .map_err(|e| PyRuntimeError::new_err(format!("{e:#}")))?;

        let d = PyDict::new(py);
        d.set_item("task", &result.task)?;
        d.set_item("agent", &result.agent)?;
        d.set_item("vmm", &result.vmm)?;
        d.set_item("outcome", format!("{:?}", result.outcome).to_uppercase())?;
        d.set_item("reward", result.reward)?;
        d.set_item("detail", &result.detail)?;
        d.set_item("booted", result.booted)?;
        d.set_item("diff_stat", &result.diff_stat)?;
        d.set_item("agent_secs", result.agent_secs)?;
        d.set_item("build_secs", result.build_secs)?;
        d.set_item("boot_secs", result.boot_secs)?;
        d.set_item("run_dir", &result.run_dir)?;
        Ok(d)
    }

    /// Fail fast on a missing vng/qemu/git rather than deep inside a build.
    #[pyfunction]
    fn check_tools() -> PyResult<()> {
        crate::vmm::check_tools().map_err(|e| PyRuntimeError::new_err(e.to_string()))
    }

    #[pymodule]
    #[pyo3(name = "_core")]
    fn floe_core(m: &Bound<'_, PyModule>) -> PyResult<()> {
        m.add_function(wrap_pyfunction!(run_trial, m)?)?;
        m.add_function(wrap_pyfunction!(check_tools, m)?)?;
        m.add("__version__", env!("CARGO_PKG_VERSION"))?;
        Ok(())
    }
}
