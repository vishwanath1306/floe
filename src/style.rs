//! Static style checking of the agent's diff.
//!
//! This is the one signal floe takes that is *not* boot evidence, so it is
//! deliberately kept subordinate to the functional result: a kernel that does
//! not boot is a worse outcome than one with a style nit, and the grader keeps
//! that ordering.
//!
//! The checker is `scripts/checkpatch.pl` from the task's own worktree, so it
//! is version-matched to the tree under test, and it is the kernel community's
//! tool rather than the harness author's taste. Note what it does and does not
//! cover: it checks formatting, not linkage or scoping, so a patch that leaks a
//! global symbol still scores 0 errors. Requirements like that belong in the
//! task's instruction and verify.sh, not here.

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::proc;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StyleReport {
    /// False when there was nothing to check, or checkpatch could not run.
    pub ran: bool,
    pub errors: u32,
    pub warnings: u32,
    /// Why it did not run, when `ran` is false.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
}

impl StyleReport {
    fn skipped(reason: &str) -> Self {
        Self {
            ran: false,
            skipped: Some(reason.to_string()),
            ..Default::default()
        }
    }
}

/// Limits a task may opt into. Absent means report-only.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct StyleGate {
    pub max_errors: Option<u32>,
    pub max_warnings: Option<u32>,
}

impl StyleGate {
    /// The first limit the report exceeds, if any.
    pub fn violation(&self, report: &StyleReport) -> Option<String> {
        if !report.ran {
            return None;
        }
        if let Some(max) = self.max_errors {
            if report.errors > max {
                return Some(format!("{} checkpatch errors (max {max})", report.errors));
            }
        }
        if let Some(max) = self.max_warnings {
            if report.warnings > max {
                return Some(format!(
                    "{} checkpatch warnings (max {max})",
                    report.warnings
                ));
            }
        }
        None
    }
}

/// Run checkpatch over `diff_path`, using the checker from the worktree.
pub fn check(ws: &Path, diff_path: &Path, log_path: &Path) -> StyleReport {
    let checkpatch = ws.join("scripts/checkpatch.pl");
    if !checkpatch.is_file() {
        return StyleReport::skipped("no scripts/checkpatch.pl in the tree");
    }
    match std::fs::metadata(diff_path) {
        Ok(m) if m.len() > 0 => {}
        _ => return StyleReport::skipped("agent produced no diff"),
    }

    // --no-tree because the diff is checked on its own; --no-signoff because a
    // worktree diff is not a submitted patch and has no trailers.
    let argv: Vec<String> = [
        "perl",
        &checkpatch.display().to_string(),
        "--no-tree",
        "--no-signoff",
        "--patch",
        &diff_path.display().to_string(),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    let step = match proc::run(proc::Spec {
        argv: &argv,
        cwd: ws,
        env: vec![],
        timeout: Duration::from_secs(120),
        log_path: log_path.to_path_buf(),
    }) {
        Ok(s) => s,
        Err(e) => return StyleReport::skipped(&format!("could not run checkpatch: {e}")),
    };

    // A nonzero exit only means the patch has findings, which is a result, not
    // a failure. Only an unparseable summary is treated as "did not run".
    match parse_totals(&step.output()) {
        Some((errors, warnings)) => StyleReport {
            ran: true,
            errors,
            warnings,
            skipped: None,
        },
        None => StyleReport::skipped("checkpatch produced no summary line"),
    }
}

/// Pull the counts out of checkpatch's summary line, e.g.
/// `total: 1 errors, 2 warnings, 3 lines checked`.
fn parse_totals(output: &str) -> Option<(u32, u32)> {
    let line = output.lines().rev().find(|l| l.starts_with("total: "))?;
    let mut errors = None;
    let mut warnings = None;
    let fields: Vec<&str> = line.trim_start_matches("total: ").split(", ").collect();
    for field in fields {
        let mut parts = field.split_whitespace();
        let (Some(n), Some(kind)) = (parts.next(), parts.next()) else {
            continue;
        };
        match kind {
            "errors" => errors = n.parse().ok(),
            "warnings" => warnings = n.parse().ok(),
            _ => {}
        }
    }
    Some((errors?, warnings?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_summary_line() {
        let out = "some finding\n\ntotal: 1 errors, 2 warnings, 3 lines checked\n";
        assert_eq!(parse_totals(out), Some((1, 2)));
    }

    #[test]
    fn parses_the_clean_case() {
        let out = "total: 0 errors, 0 warnings, 21 lines checked\n\nno obvious style problems\n";
        assert_eq!(parse_totals(out), Some((0, 0)));
    }

    #[test]
    fn parses_strict_mode_with_an_extra_field() {
        // --strict inserts a "checks" count the plain run does not have.
        let out = "total: 0 errors, 1 warnings, 2 checks, 40 lines checked\n";
        assert_eq!(parse_totals(out), Some((0, 1)));
    }

    #[test]
    fn no_summary_means_it_did_not_run() {
        assert_eq!(parse_totals("perl: command not found\n"), None);
        assert_eq!(parse_totals(""), None);
    }

    #[test]
    fn report_only_by_default() {
        // An empty gate must never fail a trial, however bad the patch is.
        let gate = StyleGate::default();
        let report = StyleReport {
            ran: true,
            errors: 99,
            warnings: 99,
            skipped: None,
        };
        assert!(gate.violation(&report).is_none());
    }

    #[test]
    fn gate_fires_only_when_exceeded() {
        let gate = StyleGate {
            max_errors: Some(0),
            max_warnings: None,
        };
        let clean = StyleReport { ran: true, errors: 0, warnings: 7, skipped: None };
        assert!(gate.violation(&clean).is_none(), "warnings ignored when unset");

        let dirty = StyleReport { ran: true, errors: 1, warnings: 0, skipped: None };
        assert!(gate.violation(&dirty).unwrap().contains("1 checkpatch errors"));
    }

    #[test]
    fn a_gate_cannot_fail_a_trial_the_checker_never_examined() {
        // If checkpatch could not run, failing the agent for it would punish
        // the agent for the harness's problem.
        let gate = StyleGate { max_errors: Some(0), max_warnings: Some(0) };
        assert!(gate.violation(&StyleReport::skipped("no perl")).is_none());
    }
}
