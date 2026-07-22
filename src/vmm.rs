//! The VMM boundary.
//!
//! virtme-ng is not itself a VMM -- it is a Python program that emits a
//! `qemu-system-x86_64` command line. An alternative backend (Cloud Hypervisor,
//! or a hand-rolled QEMU invocation) has to reproduce what vng gives us:
//!
//!   1. booting a directory as the guest root over virtio-fs, and mounting the
//!      task directory into it read-only -- no disk image to build per trial;
//!   2. kconfig and initramfs generation for an arbitrary kernel tree.
//!
//! Note what is deliberately *not* on that list: the guest's result does not
//! come back through vng at all. It arrives over vsock (see `crate::vsock`),
//! so the reporting path is independent of the VMM and survives swapping it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;

use crate::proc::{self, Step};
use crate::task::Task;
use crate::vsock::{GuestReport, Listener};

pub struct BuildResult {
    pub step: Step,
    /// None when the build did not produce a bootable image.
    pub image: Option<PathBuf>,
}

pub struct BootResult {
    pub step: Step,
    /// None when the guest never reported -- it panicked, hung, or never
    /// reached the verify script.
    pub report: Option<GuestReport>,
}

pub trait Vmm {
    fn name(&self) -> &'static str;

    /// Compile the tree in `ws` into a bootable kernel image.
    fn build(&self, task: &Task, ws: &Path, run_dir: &Path) -> Result<BuildResult>;

    /// Boot the built kernel and run `verify.sh` inside it.
    fn boot_exec(&self, task: &Task, ws: &Path, run_dir: &Path) -> Result<BootResult>;
}

/// Where the task directory is mounted inside the guest.
pub const GUEST_TASK_DIR: &str = "/task";

/// The static helper inside the rootfs that runs verify.sh and reports back.
pub const GUEST_HELPER: &str = "/bin/floe-guest";

/// Console fallback, used only when the guest's vsock send fails.
///
/// vng's own channel -- `/dev/virtio-ports/virtme.ret` -- needs udev to create
/// the named device node, which a minimal rootfs has not got. vsock is the
/// primary path; this exists so a socket failure degrades to a usable result
/// instead of an unexplained hang.
pub const EXIT_SENTINEL: &str = "FLOE_EXIT";

pub struct Vng {
    /// This host's seabios has no `bios-microvm.bin`, and the microvm machine
    /// type caps at 288 vCPUs (below this box's 368). q35 avoids both.
    pub disable_microvm: bool,
    pub ccache_dir: PathBuf,
    /// The guest root. Never the host filesystem: projecting `/` would expose
    /// the operator's home directory and credentials to agent code and make
    /// results depend on host state.
    pub rootfs: PathBuf,
}

impl Vng {
    pub fn new(ccache_dir: PathBuf, rootfs: PathBuf) -> Self {
        Self {
            disable_microvm: true,
            ccache_dir,
            rootfs,
        }
    }

    fn base_argv(&self) -> Vec<String> {
        // --verbose is not optional for us: without it vng suppresses the
        // guest console entirely, and the console is what panics, oopses and
        // hangs are graded from.
        let mut v = vec!["vng".to_string(), "--verbose".to_string()];
        if self.disable_microvm {
            v.push("--disable-microvm".to_string());
        }
        v
    }
}

impl Vmm for Vng {
    fn name(&self) -> &'static str {
        "vng"
    }

    fn build(&self, task: &Task, ws: &Path, run_dir: &Path) -> Result<BuildResult> {
        std::fs::create_dir_all(&self.ccache_dir)?;

        let mut argv = self.base_argv();
        argv.push("--build".into());
        if task.kernel.skip_modules {
            argv.push("--skip-modules".into());
        }
        // Parallelism is not ours to set: vng appends `-j $(os.cpu_count())`
        // after any Makefile variables we pass, so a `-j` of ours would be
        // overridden anyway. Fine for one trial on a 368-core box; bounding it
        // matters once trials run concurrently, and would mean driving `make`
        // directly instead of `vng --build`.
        //
        // Every run builds in a fresh worktree, so there is no incremental
        // object reuse -- ccache is the only thing standing between us and a
        // full rebuild each iteration. CCACHE_DIR alone does nothing; the
        // compiler has to be wrapped. vng forwards positional arguments to
        // make as variables, which is exactly the hook we need.
        if have_ccache() {
            argv.push("CC=ccache gcc".into());
        }
        // The guest reports its result over vsock, so the kernel under test
        // must be able to speak it regardless of what the task's tree
        // defaults to. These override the generated config.
        for item in ["CONFIG_VSOCKETS=y", "CONFIG_VIRTIO_VSOCKETS=y"] {
            argv.push("--configitem".into());
            argv.push(item.into());
        }

        let step = proc::run(proc::Spec {
            argv: &argv,
            cwd: ws,
            env: ccache_env(&self.ccache_dir, ws),
            timeout: Duration::from_secs(task.build_timeout_sec),
            log_path: run_dir.join("build.log"),
        })?;

        let image = ws.join("arch/x86/boot/bzImage");
        let image = image.exists().then_some(image);
        Ok(BuildResult { step, image })
    }

    fn boot_exec(&self, task: &Task, ws: &Path, run_dir: &Path) -> Result<BootResult> {
        anyhow::ensure!(
            self.rootfs.join("bin/busybox").exists(),
            "no guest rootfs at {} -- run scripts/build-rootfs.sh",
            self.rootfs.display()
        );
        anyhow::ensure!(
            self.rootfs.join(GUEST_HELPER.trim_start_matches('/')).exists(),
            "no guest helper in {} -- rebuild the rootfs",
            self.rootfs.display()
        );

        // Listen before the guest exists, so a fast guest cannot report into a
        // socket that is not up yet.
        let listener = Listener::bind(crate::vsock::default_port_hint())?;
        let timeout = Duration::from_secs(task.verify_timeout_sec);

        let mut argv = self.base_argv();
        argv.extend([
            "--cpus".into(),
            task.kernel.guest_cpus.to_string(),
            "--memory".into(),
            task.kernel.guest_memory.clone(),
            // The guest sees this root and nothing else of the host...
            "--root".into(),
            self.rootfs.display().to_string(),
            // ...plus the task directory, read-only.
            format!("--rodir={}={}", GUEST_TASK_DIR, task.dir.display()),
            // The `=` form is required: argparse rejects a separate value
            // beginning with `-`, which every qemu device string does.
            format!(
                "--qemu-opts=-device vhost-vsock-pci,guest-cid={}",
                listener.guest_cid
            ),
            "--run".into(),
            ws.display().to_string(),
            "--exec".into(),
            format!(
                "{GUEST_HELPER} {} {GUEST_TASK_DIR}/verify.sh",
                listener.port
            ),
        ]);

        // The guest reports while the VM is still running, so the listener has
        // to be serviced concurrently with it. Once the VM is gone nothing more
        // can arrive; the flag stops the collector from waiting out the full
        // timeout on a guest that never booted.
        let vm_gone = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let collector = {
            let vm_gone = std::sync::Arc::clone(&vm_gone);
            std::thread::spawn(move || listener.recv(timeout, &vm_gone))
        };

        let step = proc::run(proc::Spec {
            argv: &argv,
            cwd: ws,
            env: vec![],
            timeout,
            log_path: run_dir.join("console.log"),
        });
        vm_gone.store(true, std::sync::atomic::Ordering::Relaxed);
        let step = step?;

        let report = collector
            .join()
            .map_err(|_| anyhow::anyhow!("vsock collector panicked"))??;

        // Fall back to the console sentinel if vsock did not deliver: the
        // guest prints it when its socket send fails, and a result recovered
        // from the console still beats declaring the trial broken.
        let report = report.or_else(|| {
            parse_guest_exit(&step.output()).map(|exit| GuestReport {
                exit,
                output: String::new(),
            })
        });

        Ok(BootResult { step, report })
    }
}

/// Recover the guest's exit status from the console.
pub fn parse_guest_exit(console: &str) -> Option<i32> {
    // Last occurrence wins: the sentinel echoes to the serial console, which
    // can also carry the command itself back during boot.
    console
        .lines()
        .filter_map(|line| line.rsplit_once(&format!("{EXIT_SENTINEL}=")))
        .filter_map(|(_, rest)| {
            let digits: String = rest.trim().chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.parse::<i32>().ok()
        })
        .last()
}

/// Make ccache hits survive across runs.
///
/// Every trial builds in a differently-named worktree, so the absolute paths
/// baked into each compile differ and ccache misses on everything -- measured
/// at 2.5% hits before this. `CCACHE_BASEDIR` makes ccache rewrite paths under
/// the worktree to relative form, and `NOHASHDIR` stops the build directory
/// itself from entering the hash. The sloppiness set covers what a kernel
/// build does that ccache is conservative about by default.
fn ccache_env(ccache_dir: &Path, ws: &Path) -> Vec<(String, String)> {
    vec![
        ("CCACHE_DIR".into(), ccache_dir.display().to_string()),
        ("CCACHE_BASEDIR".into(), ws.display().to_string()),
        ("CCACHE_NOHASHDIR".into(), "1".into()),
        (
            "CCACHE_SLOPPINESS".into(),
            "file_macro,time_macros,include_file_mtime,include_file_ctime,locale,pch_defines"
                .into(),
        ),
        // Deterministic stamp: otherwise every build embeds a different date
        // and nothing downstream of it can ever be reused.
        ("KBUILD_BUILD_TIMESTAMP".into(), "floe".into()),
    ]
}

fn on_path(tool: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|d| d.join(tool).is_file()))
        .unwrap_or(false)
}

/// ccache is an optimisation, not a requirement -- builds are correct without
/// it, just slower.
fn have_ccache() -> bool {
    on_path("ccache")
}

/// Preflight so a missing dependency fails immediately rather than as a
/// mysterious build error twenty minutes in.
pub fn check_tools() -> Result<()> {
    for tool in ["vng", "git", "qemu-system-x86_64"] {
        anyhow::ensure!(on_path(tool), "required tool not on PATH: {tool}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proc::Exit;

    #[test]
    fn reads_guest_exit_from_console() {
        assert_eq!(parse_guest_exit("OK\nFLOE_EXIT=0\n"), Some(0));
        assert_eq!(parse_guest_exit("BAD_VALUE: 7\nFLOE_EXIT=1\n"), Some(1));
    }

    #[test]
    fn absent_sentinel_means_the_guest_never_reported() {
        // The distinction the whole scheme exists for: a guest that died has
        // no exit status, which is not the same as exiting nonzero.
        assert_eq!(parse_guest_exit("Kernel panic - not syncing: oh no"), None);
        assert_eq!(parse_guest_exit(""), None);
    }

    #[test]
    fn tolerates_serial_noise_around_the_sentinel() {
        // The console interleaves kernel printk with our output.
        let console = "[    2.1] virtme-init: run\nFLOE_EXIT=3\r\n[    2.2] reboot";
        assert_eq!(parse_guest_exit(console), Some(3));
    }

    #[test]
    fn command_echo_does_not_beat_the_real_result() {
        // Boot can echo the exec string itself; the real value comes later.
        let console = "cmdline: bash /task/verify.sh; echo FLOE_EXIT=$?\nFLOE_EXIT=0\n";
        assert_eq!(parse_guest_exit(console), Some(0));
    }

    #[test]
    fn build_result_reports_missing_image() {
        let r = BuildResult {
            step: Step {
                exit: Exit::Code(0),
                log_path: PathBuf::from("/dev/null"),
                elapsed: Duration::ZERO,
            },
            image: None,
        };
        assert!(r.image.is_none());
        assert!(r.step.exit.is_success());
    }
}
