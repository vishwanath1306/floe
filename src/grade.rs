//! Outcome classification.
//!
//! This is the part that most distinguishes floe from a container harness.
//! Harbor's verifier reads a reward file the tests wrote *inside* the
//! environment. If an agent's kernel panics there is no inside -- nothing runs,
//! nothing writes, and the trial looks like broken infrastructure rather than
//! a failed attempt.
//!
//! So reward is computed on the host from evidence: the guest's exit status
//! plus the serial console. That makes "panicked on boot" a scored outcome,
//! which for kernel work is often the most informative result available.

use serde::{Deserialize, Serialize};

use crate::proc::Exit;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Outcome {
    /// verify.sh succeeded on the agent's kernel.
    Pass,
    /// The agent's changes did not compile.
    FailBuild,
    /// The build exceeded its deadline.
    FailBuildTimeout,
    /// The kernel built but did not produce a bootable image.
    FailNoImage,
    /// The guest panicked during boot.
    FailPanic,
    /// The guest hit an oops, BUG, or WARN.
    FailOops,
    /// The guest never finished -- no panic on the console, just silence.
    FailHang,
    /// The kernel booted fine; the task's own check failed.
    FailTest,
}

impl Outcome {
    /// Binary reward. Kept separate from the outcome so richer shaping (partial
    /// credit for "booted but wrong", say) is a scoring change, not a
    /// classification change.
    pub fn reward(&self) -> f64 {
        match self {
            Outcome::Pass => 1.0,
            _ => 0.0,
        }
    }

    pub fn booted(&self) -> bool {
        matches!(self, Outcome::Pass | Outcome::FailTest | Outcome::FailOops)
    }
}

/// Console signatures, most severe first. A panic usually also produces a
/// hang, so console evidence outranks the timeout when deciding why.
const CONSOLE_SIGNATURES: &[(Outcome, &str)] = &[
    (Outcome::FailPanic, "Kernel panic - not syncing"),
    (Outcome::FailOops, "BUG: kernel NULL pointer dereference"),
    (Outcome::FailOops, "general protection fault"),
    (Outcome::FailOops, "Oops:"),
    (Outcome::FailOops, "WARNING: CPU:"),
];

pub struct Evidence<'a> {
    pub build: &'a Exit,
    pub image_present: bool,
    /// How the VMM process itself ended. None when no boot was attempted.
    /// Used only to tell a hang from a clean finish -- the guest's own result
    /// comes from `guest_exit`, because the VMM exiting 0 says nothing about
    /// whether anything inside it worked.
    pub boot: Option<&'a Exit>,
    /// verify.sh's status, recovered from the console sentinel. None means the
    /// guest never got far enough to report.
    pub guest_exit: Option<i32>,
    pub console: &'a str,
}

pub struct Verdict {
    pub outcome: Outcome,
    pub detail: String,
}

pub fn grade(ev: Evidence<'_>) -> Verdict {
    let v = |outcome, detail: String| Verdict { outcome, detail };

    match ev.build {
        Exit::Timeout => {
            return v(
                Outcome::FailBuildTimeout,
                "kernel build exceeded its deadline".into(),
            )
        }
        Exit::Signal(s) => {
            return v(
                Outcome::FailBuild,
                format!("kernel build killed by signal {s}"),
            )
        }
        Exit::Code(c) if *c != 0 => {
            return v(Outcome::FailBuild, format!("kernel build exited {c}"))
        }
        Exit::Code(_) => {}
    }

    if !ev.image_present {
        return v(
            Outcome::FailNoImage,
            "build succeeded but produced no bzImage".into(),
        );
    }

    let boot = match ev.boot {
        Some(b) => b,
        None => return v(Outcome::FailNoImage, "boot was never attempted".into()),
    };

    // Console evidence first: it explains *why*, where an exit status only
    // says *that*.
    for (outcome, needle) in CONSOLE_SIGNATURES {
        if let Some(line) = find_line(ev.console, needle) {
            return v(*outcome, format!("guest console: {line}"));
        }
    }

    // The guest's own report is authoritative when present. The VMM's exit
    // code is not a substitute: vng returns nonzero for guests that ran the
    // task fine but could not power down cleanly.
    match ev.guest_exit {
        Some(0) => {
            return v(
                Outcome::Pass,
                "verify.sh passed inside the agent-built kernel".into(),
            )
        }
        Some(c) => {
            return v(
                Outcome::FailTest,
                format!("verify.sh exited {c} inside the guest"),
            )
        }
        None => {}
    }

    // No fault on the console and no result from the guest: it never got there.
    match boot {
        Exit::Timeout => v(
            Outcome::FailHang,
            "guest did not finish before the deadline and printed no fault".into(),
        ),
        Exit::Signal(s) => v(Outcome::FailHang, format!("VMM killed by signal {s}")),
        _ => v(
            Outcome::FailHang,
            "guest exited without reporting a result -- it never reached verify.sh".into(),
        ),
    }
}

/// Return the whole console line containing `needle`, trimmed and capped --
/// the surrounding text is what makes an oops readable in a summary.
fn find_line(console: &str, needle: &str) -> Option<String> {
    console.lines().find(|l| l.contains(needle)).map(|l| {
        let t = l.trim();
        if t.chars().count() > 200 {
            t.chars().take(200).collect::<String>() + "..."
        } else {
            t.to_string()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev<'a>(build: &'a Exit, boot: Option<&'a Exit>, console: &'a str) -> Evidence<'a> {
        Evidence {
            build,
            image_present: true,
            boot,
            guest_exit: crate::vmm::parse_guest_exit(console),
            console,
        }
    }

    #[test]
    fn build_failure_short_circuits() {
        let b = Exit::Code(2);
        assert_eq!(grade(ev(&b, None, "")).outcome, Outcome::FailBuild);
    }

    #[test]
    fn panic_beats_timeout() {
        // The characteristic kernel failure: it panics, then never returns.
        // Reporting this as a hang would hide the actual cause.
        let ok = Exit::Code(0);
        let boot = Exit::Timeout;
        let console = "[    0.5] Kernel panic - not syncing: VFS: Unable to mount root fs";
        let verdict = grade(ev(&ok, Some(&boot), console));
        assert_eq!(verdict.outcome, Outcome::FailPanic);
        assert!(verdict.detail.contains("VFS: Unable to mount root fs"));
    }

    #[test]
    fn silent_timeout_is_a_hang() {
        let ok = Exit::Code(0);
        let boot = Exit::Timeout;
        assert_eq!(
            grade(ev(&ok, Some(&boot), "booting...")).outcome,
            Outcome::FailHang
        );
    }

    #[test]
    fn nonzero_guest_exit_is_a_test_failure_not_infra() {
        let ok = Exit::Code(0);
        let boot = Exit::Code(0);
        let console = "BAD_VALUE: reads '7', expected '42'\nFLOE_EXIT=1\n";
        assert_eq!(grade(ev(&ok, Some(&boot), console)).outcome, Outcome::FailTest);
    }

    #[test]
    fn clean_run_passes() {
        let ok = Exit::Code(0);
        let boot = Exit::Code(0);
        let verdict = grade(ev(&ok, Some(&boot), "OK\nFLOE_EXIT=0\n"));
        assert_eq!(verdict.outcome, Outcome::Pass);
        assert_eq!(verdict.outcome.reward(), 1.0);
    }

    #[test]
    fn warn_on_console_is_reported_even_when_tests_pass() {
        // A kernel that boots and passes but WARNs is not a clean result.
        let ok = Exit::Code(0);
        let boot = Exit::Code(0);
        let console = "WARNING: CPU: 1 PID: 42 at kernel/sched/core.c:1234\nFLOE_EXIT=0\n";
        assert_eq!(grade(ev(&ok, Some(&boot), console)).outcome, Outcome::FailOops);
    }

    #[test]
    fn vmm_exit_code_does_not_override_the_guest() {
        // vng returns nonzero when a minimal guest cannot power down cleanly,
        // even though verify.sh succeeded. Trusting the VMM here would fail
        // every passing trial.
        let ok = Exit::Code(0);
        let boot = Exit::Code(255);
        let verdict = grade(ev(&ok, Some(&boot), "OK\nFLOE_EXIT=0\n"));
        assert_eq!(verdict.outcome, Outcome::Pass);
    }

    #[test]
    fn guest_that_never_reported_is_a_hang_not_a_pass() {
        let ok = Exit::Code(0);
        let boot = Exit::Code(0);
        assert_eq!(
            grade(ev(&ok, Some(&boot), "booting...")).outcome,
            Outcome::FailHang
        );
    }
}
