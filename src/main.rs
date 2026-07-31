//! Standalone CLI over the same core the Python module uses.
//!
//! Exists so the harness can be debugged without a Python build step in the
//! way -- and so a broken PyO3 build never blocks running a trial.

use std::path::PathBuf;
use std::process::ExitCode;

use floe_core::trial::{self, Agent, Config};

const USAGE: &str = "\
usage: floe <task-dir> [--agent claude|solution|none] [--keep] [--json]

  --agent     what mutates the workspace (default: claude)
              solution = the task's reference patch (no tokens spent)
              none     = change nothing; a correct task must not pass this
  --keep      keep the worktree for inspection
  --json      print the result as JSON only

env:
  FLOE_KERNEL_SRC   kernel git clone   (default: ./kernel-src)
  FLOE_RUNS_DIR     run output         (default: ./runs)
  FLOE_CCACHE_DIR   shared ccache      (default: ./.ccache)
  FLOE_ROOTFS       minimal guest root (default: ./rootfs)
";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut task_dir: Option<PathBuf> = None;
    let mut agent = "claude".to_string();
    let mut keep = false;
    let mut json_only = false;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "--keep" => keep = true,
            "--json" => json_only = true,
            "--agent" => match args.next() {
                Some(v) => agent = v,
                None => return fail("--agent needs a value"),
            },
            other if other.starts_with('-') => {
                return fail(&format!("unknown flag {other}"));
            }
            other => task_dir = Some(PathBuf::from(other)),
        }
    }

    let Some(task_dir) = task_dir else {
        eprint!("{USAGE}");
        return ExitCode::from(2);
    };

    let agent = match Agent::parse(&agent) {
        Ok(a) => a,
        Err(e) => return fail(&e.to_string()),
    };

    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let cfg = Config {
        kernel_src: env_path("FLOE_KERNEL_SRC", root.join("kernel-src")),
        runs_dir: env_path("FLOE_RUNS_DIR", root.join("runs")),
        ccache_dir: env_path("FLOE_CCACHE_DIR", root.join(".ccache")),
        rootfs: env_path("FLOE_ROOTFS", root.join("rootfs")),
        keep,
    };

    if !json_only {
        eprintln!(
            "[floe] task={} agent={}",
            task_dir.display(),
            agent.as_str()
        );
    }

    match trial::run_trial(&task_dir, agent, &cfg) {
        Ok(r) => {
            if json_only {
                println!("{}", serde_json::to_string_pretty(&r).unwrap_or_default());
            } else {
                println!();
                println!("  outcome   {:?}", r.outcome);
                println!("  reward    {}", r.reward);
                println!("  detail    {}", r.detail);
                println!(
                    "  diff      {}",
                    if r.diff_stat.is_empty() {
                        "(agent changed nothing)"
                    } else {
                        &r.diff_stat
                    }
                );
                println!(
                    "  timing    agent {:.0}s  build {:.0}s  boot {:.0}s",
                    r.agent_secs, r.build_secs, r.boot_secs
                );
                println!("  evidence  {}", r.run_dir);
            }
            if r.reward > 0.0 {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(e) => fail(&format!("{e:#}")),
    }
}

fn env_path(key: &str, default: PathBuf) -> PathBuf {
    std::env::var_os(key).map(PathBuf::from).unwrap_or(default)
}

fn fail(msg: &str) -> ExitCode {
    eprintln!("[floe] error: {msg}");
    ExitCode::from(2)
}
