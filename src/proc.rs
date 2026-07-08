//! Process supervision.
//!
//! Every external step in a trial -- the agent, the kernel build, the guest
//! boot -- is a child process that may hang, and a hung guest must not leave
//! an orphaned QEMU behind. Children are therefore put in their own process
//! group and killed by group on timeout.
//!
//! Both stdout and stderr are wired to the same log file so the kernel's
//! serial output interleaves correctly with everything else. Grading reads
//! that file back; ordering matters, because a panic message is only
//! interpretable relative to what preceded it.

use std::fs::File;
use std::io;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(100);
const SIGKILL_GRACE: Duration = Duration::from_secs(5);

/// How a supervised child finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exit {
    /// Ran to completion with this exit code.
    Code(i32),
    /// Killed by a signal (a QEMU segfault, say).
    Signal(i32),
    /// Exceeded its deadline and was killed. Distinct from a nonzero exit:
    /// for a kernel boot, "hung" and "failed" are different findings.
    Timeout,
}

impl Exit {
    pub fn is_success(&self) -> bool {
        matches!(self, Exit::Code(0))
    }

    /// Exit code if the process exited normally, else None.
    pub fn code(&self) -> Option<i32> {
        match self {
            Exit::Code(c) => Some(*c),
            _ => None,
        }
    }
}

pub struct Step {
    pub exit: Exit,
    pub log_path: PathBuf,
    pub elapsed: Duration,
}

impl Step {
    /// Read back what the child wrote. Lossy: a guest can emit non-UTF-8 on
    /// the serial line and that must not fail the trial.
    pub fn output(&self) -> String {
        std::fs::read(&self.log_path)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default()
    }
}

pub struct Spec<'a> {
    pub argv: &'a [String],
    pub cwd: &'a Path,
    pub env: Vec<(String, String)>,
    pub timeout: Duration,
    pub log_path: PathBuf,
}

pub fn run(spec: Spec<'_>) -> io::Result<Step> {
    let (program, args) = spec
        .argv
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty argv"))?;

    let log = File::create(&spec.log_path)?;
    let log_err = log.try_clone()?;

    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(spec.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    for (k, v) in &spec.env {
        cmd.env(k, v);
    }
    // Own process group: lets us reap the whole QEMU/virtiofsd tree on timeout.
    cmd.process_group(0);

    let started = Instant::now();
    let mut child = cmd.spawn()?;
    let pgid = child.id() as i32;

    let exit = loop {
        match child.try_wait()? {
            Some(status) => {
                break match (status.code(), status.signal()) {
                    (Some(c), _) => Exit::Code(c),
                    (None, Some(s)) => Exit::Signal(s),
                    _ => Exit::Code(-1),
                };
            }
            None => {
                if started.elapsed() >= spec.timeout {
                    kill_group(pgid, libc::SIGTERM);
                    let deadline = Instant::now() + SIGKILL_GRACE;
                    while Instant::now() < deadline {
                        if child.try_wait()?.is_some() {
                            break;
                        }
                        std::thread::sleep(POLL_INTERVAL);
                    }
                    kill_group(pgid, libc::SIGKILL);
                    let _ = child.wait();
                    break Exit::Timeout;
                }
                std::thread::sleep(POLL_INTERVAL);
            }
        }
    };

    Ok(Step {
        exit,
        log_path: spec.log_path,
        elapsed: started.elapsed(),
    })
}

fn kill_group(pgid: i32, sig: i32) {
    // Negative pid targets the group. Failure just means it already exited.
    unsafe { libc::kill(-pgid, sig) };
}
