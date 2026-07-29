//! One trial: mutate a worktree, build it, boot it, grade it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::grade::{self, Evidence, Outcome};
use crate::proc::{self, Exit};
use crate::task::Task;
use crate::vmm::{Vmm, Vng};
use crate::workspace::Workspace;

/// What mutates the workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    /// Headless Claude Code.
    Claude,
    /// The task's reference patch -- exercises the harness without spending
    /// tokens, and proves a task is solvable before an agent ever sees it.
    Solution,
    /// Change nothing. The control: a correct task must NOT pass this.
    None,
}

impl Agent {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "claude" => Ok(Agent::Claude),
            "solution" | "oracle" => Ok(Agent::Solution),
            "none" | "nop" => Ok(Agent::None),
            other => anyhow::bail!("unknown agent {other:?} (claude|solution|none)"),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Solution => "solution",
            Agent::None => "none",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrialResult {
    pub task: String,
    pub agent: String,
    pub vmm: String,
    pub outcome: Outcome,
    pub reward: f64,
    pub detail: String,
    pub booted: bool,
    /// Empty when the agent made no changes -- worth surfacing on its own.
    pub diff_stat: String,
    /// What verify.sh printed inside the guest.
    pub verify_output: String,
    pub agent_secs: f64,
    pub build_secs: f64,
    pub boot_secs: f64,
    pub run_dir: String,
}

pub struct Config {
    pub kernel_src: PathBuf,
    pub runs_dir: PathBuf,
    pub ccache_dir: PathBuf,
    /// Minimal guest root, built by scripts/build-rootfs.sh. Deliberately not
    /// the host filesystem.
    pub rootfs: PathBuf,
    pub keep: bool,
}

pub fn run_trial(task_dir: &Path, agent: Agent, cfg: &Config) -> Result<TrialResult> {
    crate::vmm::check_tools()?;
    let task = Task::load(task_dir)?;

    let run_dir = cfg.runs_dir.join(format!(
        "{}-{}-{}",
        task_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "task".into()),
        agent.as_str(),
        timestamp()
    ));
    std::fs::create_dir_all(&run_dir)
        .with_context(|| format!("creating {}", run_dir.display()))?;

    let ws = Workspace::create(&cfg.kernel_src, &run_dir, &task.kernel.base_ref, cfg.keep)?;
    let vmm = Vng::new(cfg.ccache_dir.clone(), cfg.rootfs.clone());

    // --- setup phase -----------------------------------------------------
    // Lets a task hand the agent a tree that is already broken. Committing
    // afterwards keeps the injected breakage out of the agent's diff, so the
    // run record shows what the agent did and not what it was given.
    if let Some(setup) = task.setup() {
        let step = proc::run(proc::Spec {
            argv: &setup.argv(),
            cwd: &ws.path,
            env: vec![],
            timeout: Duration::from_secs(600),
            log_path: run_dir.join("setup.log"),
        })?;
        anyhow::ensure!(
            step.exit.is_success(),
            "task setup ({}) failed ({:?}); see {}",
            setup.path().display(),
            step.exit,
            step.log_path.display()
        );
        ws.commit_all("floe: task setup")?;
    }

    // --- agent phase -----------------------------------------------------
    let agent_secs = run_agent(&task, agent, &ws.path, &run_dir)?;

    let diff = ws.diff();
    std::fs::write(run_dir.join("agent.diff"), &diff)?;
    let diff_stat = summarize_diff(&diff);

    // --- build phase -----------------------------------------------------
    let build = vmm.build(&task, &ws.path, &run_dir)?;
    let build_secs = build.step.elapsed.as_secs_f64();

    // --- boot + verify ---------------------------------------------------
    // Only attempted if there is something to boot; otherwise the grader
    // would misread an absent console as a silent hang.
    let (boot_exit, console, guest_exit, guest_output, boot_secs) =
        if build.step.exit.is_success() && build.image.is_some() {
            let boot = vmm.boot_exec(&task, &ws.path, &run_dir)?;
            let secs = boot.step.elapsed.as_secs_f64();
            let (exit, output) = match &boot.report {
                Some(r) => (Some(r.exit), r.output.clone()),
                None => (None, String::new()),
            };
            (
                Some(boot.step.exit.clone()),
                boot.step.output(),
                exit,
                output,
                secs,
            )
        } else {
            (None, String::new(), None, String::new(), 0.0)
        };

    // What the guest itself said, kept apart from the console it shares with
    // the kernel. This is the artifact a task author actually wants to read.
    if !guest_output.is_empty() {
        std::fs::write(run_dir.join("verify.out"), &guest_output)?;
    }

    let verdict = grade::grade(Evidence {
        build: &build.step.exit,
        image_present: build.image.is_some(),
        boot: boot_exit.as_ref(),
        guest_exit,
        console: &console,
    });

    let result = TrialResult {
        task: task.meta.name.clone(),
        agent: agent.as_str().to_string(),
        vmm: vmm.name().to_string(),
        reward: verdict.outcome.reward(),
        booted: verdict.outcome.booted(),
        outcome: verdict.outcome,
        detail: verdict.detail,
        verify_output: guest_output.trim().to_string(),
        diff_stat,
        agent_secs,
        build_secs,
        boot_secs,
        run_dir: run_dir.display().to_string(),
    };

    std::fs::write(
        run_dir.join("reward.json"),
        serde_json::to_string_pretty(&result)? + "\n",
    )?;
    Ok(result)
}

fn run_agent(task: &Task, agent: Agent, ws: &Path, run_dir: &Path) -> Result<f64> {
    let argv: Vec<String> = match agent {
        Agent::None => return Ok(0.0),
        Agent::Solution => {
            let solution = task
                .solution()
                .ok_or_else(|| anyhow::anyhow!("no solve.patch or solve.sh in {}",
                                               task.dir.join("solution").display()))?;
            solution.argv()
        }
        Agent::Claude => vec![
            "claude".into(),
            "-p".into(),
            task.instruction.clone(),
            "--permission-mode".into(),
            "bypassPermissions".into(),
        ],
    };

    let step = proc::run(proc::Spec {
        argv: &argv,
        cwd: ws,
        env: vec![],
        timeout: Duration::from_secs(task.agent_timeout_sec),
        log_path: run_dir.join("agent.log"),
    })?;

    // A failed agent is not a harness error: we still build and boot whatever
    // it left behind, because a partial edit that panics is a real result.
    if !step.exit.is_success() {
        let how = match step.exit {
            Exit::Timeout => "timed out".to_string(),
            Exit::Signal(s) => format!("died on signal {s}"),
            Exit::Code(c) => format!("exited {c}"),
        };
        eprintln!("[floe] warning: agent {how}; grading the tree as left");
    }
    Ok(step.elapsed.as_secs_f64())
}

fn summarize_diff(diff: &str) -> String {
    if diff.trim().is_empty() {
        return String::new();
    }
    let files = diff.lines().filter(|l| l.starts_with("+++ ")).count();
    let added = diff
        .lines()
        .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
        .count();
    let removed = diff
        .lines()
        .filter(|l| l.starts_with('-') && !l.starts_with("---"))
        .count();
    format!("{files} file(s), +{added}/-{removed}")
}

fn timestamp() -> String {
    // Seconds since epoch: sortable, no chrono dependency.
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_aliases() {
        assert_eq!(Agent::parse("oracle").unwrap(), Agent::Solution);
        assert_eq!(Agent::parse("nop").unwrap(), Agent::None);
        assert!(Agent::parse("gpt").is_err());
    }

    #[test]
    fn empty_diff_is_empty_not_zeroes() {
        // Distinguishing "agent did nothing" from "agent changed 0 lines"
        // matters when triaging a run.
        assert_eq!(summarize_diff("   \n"), "");
    }

    #[test]
    fn diff_stat_ignores_headers() {
        let d = "--- a/x\n+++ b/x\n-old\n+new\n+more\n";
        assert_eq!(summarize_diff(d), "1 file(s), +2/-1");
    }
}
