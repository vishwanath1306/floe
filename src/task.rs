//! Task definitions.
//!
//! The layout is deliberately Harbor-compatible -- `task.toml`,
//! `instruction.md`, a solution dir -- so tasks can move between harnesses.
//! What differs is the `[kernel]` block. Harbor's `[environment]` describes a
//! Dockerfile build context; a kernel task's environment identity is
//! (base tree, kconfig, toolchain) instead, which does not fit that shape.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

fn default_base_ref() -> String {
    "HEAD".to_string()
}
fn default_true() -> bool {
    true
}
fn default_guest_cpus() -> u32 {
    4
}
fn default_guest_memory() -> String {
    "2G".to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TaskMeta {
    pub name: String,
    #[serde(default)]
    pub version: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct KernelSpec {
    /// Git ref the run's worktree is created from.
    #[serde(default = "default_base_ref")]
    pub base_ref: String,
    /// Skip module builds. Most tasks only need the bzImage; skipping is a
    /// large speedup and modules are useless without a matching install step.
    #[serde(default = "default_true")]
    pub skip_modules: bool,
    #[serde(default = "default_guest_cpus")]
    pub guest_cpus: u32,
    #[serde(default = "default_guest_memory")]
    pub guest_memory: String,
}

impl Default for KernelSpec {
    fn default() -> Self {
        Self {
            base_ref: default_base_ref(),
            skip_modules: true,
            guest_cpus: default_guest_cpus(),
            guest_memory: default_guest_memory(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Timeout {
    pub timeout_sec: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct RawTask {
    task: TaskMeta,
    #[serde(default)]
    kernel: KernelSpec,
    #[serde(default)]
    style: crate::style::StyleGate,
    build: Option<Timeout>,
    agent: Option<Timeout>,
    verifier: Option<Timeout>,
}

/// A canned change to the worktree: either a diff or a script.
#[derive(Debug, Clone)]
pub enum Mutation {
    Patch(PathBuf),
    Script(PathBuf),
}

impl Mutation {
    fn find(dir: &Path, stem: &str) -> Option<Self> {
        let patch = dir.join(format!("{stem}.patch"));
        if patch.is_file() {
            return Some(Mutation::Patch(patch));
        }
        let script = dir.join(format!("{stem}.sh"));
        script.is_file().then(|| Mutation::Script(script))
    }

    /// The command that applies it, run in the worktree.
    pub fn argv(&self) -> Vec<String> {
        match self {
            Mutation::Patch(p) => vec![
                "git".into(),
                "apply".into(),
                "--verbose".into(),
                p.display().to_string(),
            ],
            Mutation::Script(p) => vec!["bash".into(), p.display().to_string()],
        }
    }

    pub fn path(&self) -> &Path {
        match self {
            Mutation::Patch(p) | Mutation::Script(p) => p,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Task {
    pub dir: PathBuf,
    pub meta: TaskMeta,
    pub kernel: KernelSpec,
    pub build_timeout_sec: u64,
    pub agent_timeout_sec: u64,
    pub verify_timeout_sec: u64,
    pub instruction: String,
    /// Absent limits mean checkpatch is reported but never fails a trial.
    pub style: crate::style::StyleGate,
}

impl Task {
    pub fn load(dir: &Path) -> Result<Self> {
        let dir = dir
            .canonicalize()
            .with_context(|| format!("task dir not found: {}", dir.display()))?;

        let toml_path = dir.join("task.toml");
        let raw: RawTask = toml::from_str(
            &std::fs::read_to_string(&toml_path)
                .with_context(|| format!("reading {}", toml_path.display()))?,
        )
        .with_context(|| format!("parsing {}", toml_path.display()))?;

        let instruction_path = dir.join("instruction.md");
        let instruction = std::fs::read_to_string(&instruction_path)
            .with_context(|| format!("reading {}", instruction_path.display()))?;

        Ok(Self {
            meta: raw.task,
            kernel: raw.kernel,
            build_timeout_sec: raw.build.map(|t| t.timeout_sec).unwrap_or(3600),
            agent_timeout_sec: raw.agent.map(|t| t.timeout_sec).unwrap_or(1800),
            verify_timeout_sec: raw.verifier.map(|t| t.timeout_sec).unwrap_or(300),
            instruction,
            style: raw.style,
            dir,
        })
    }

    /// Runs inside the guest, on the kernel the agent built.
    pub fn verify_script(&self) -> PathBuf {
        self.dir.join("verify.sh")
    }

    /// The oracle: a known-good change, used to test the harness without
    /// spending agent tokens. Harbor calls this the `oracle` agent.
    ///
    /// A patch is preferred over a script. A script has to reproduce the edit
    /// through string surgery, which means escaping the target language inside
    /// the shell inside whatever wrote the script -- three layers that quietly
    /// disagree. A diff has no escaping layer and reads as the change itself.
    pub fn solution(&self) -> Option<Mutation> {
        Mutation::find(&self.dir.join("solution"), "solve")
    }

    /// Optional. Runs in the worktree before the agent, letting a task ship a
    /// deliberately broken tree -- which is what most real kernel work looks
    /// like. Its changes are committed, so they stay out of the agent's diff.
    pub fn setup(&self) -> Option<Mutation> {
        Mutation::find(&self.dir, "setup")
    }
}
