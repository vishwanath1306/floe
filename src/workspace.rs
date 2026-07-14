//! Per-run kernel worktrees.
//!
//! Each trial gets its own `git worktree` off the shared kernel clone: an
//! isolated tree that costs a checkout rather than a 6 GB copy, and shares the
//! object store. Build artifacts stay per-worktree; the expensive shared state
//! is ccache, which lives outside and is what actually makes repeat builds fast.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

pub struct Workspace {
    pub path: PathBuf,
    kernel_src: PathBuf,
    keep: bool,
}

impl Workspace {
    pub fn create(kernel_src: &Path, run_dir: &Path, base_ref: &str, keep: bool) -> Result<Self> {
        if !kernel_src.join(".git").exists() {
            bail!(
                "{} is not a git repository -- floe creates a worktree per run",
                kernel_src.display()
            );
        }
        let path = run_dir.join("workspace");
        let out = Command::new("git")
            .args(["-C", &kernel_src.display().to_string()])
            .args(["worktree", "add", "--detach"])
            .arg(&path)
            .arg(base_ref)
            .output()
            .context("spawning git worktree add")?;
        if !out.status.success() {
            bail!(
                "git worktree add {base_ref} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(Self {
            path,
            kernel_src: kernel_src.to_path_buf(),
            keep,
        })
    }

    /// The agent's changes, for the run record. A trial where the agent edited
    /// nothing is a distinct kind of failure from one where it edited the
    /// wrong thing, and the diff is what tells them apart.
    pub fn diff(&self) -> String {
        Command::new("git")
            .args(["-C", &self.path.display().to_string(), "diff"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    }

    /// Fold current changes into a commit so later diffs exclude them.
    /// Identity is passed per-command: the run must not depend on, or touch,
    /// the operator's git config.
    pub fn commit_all(&self, message: &str) -> Result<()> {
        let git = |args: &[&str]| -> Result<()> {
            let out = Command::new("git")
                .args(["-C", &self.path.display().to_string()])
                .args(["-c", "user.name=floe", "-c", "user.email=floe@invalid"])
                .args(args)
                .output()
                .context("spawning git")?;
            if !out.status.success() {
                bail!(
                    "git {:?} failed: {}",
                    args,
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            Ok(())
        };
        git(&["add", "-A"])?;
        git(&["commit", "--allow-empty", "-m", message])
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        // Best effort: a leaked worktree wastes disk but must never mask the
        // trial's real result by panicking during unwind.
        let _ = Command::new("git")
            .args(["-C", &self.kernel_src.display().to_string()])
            .args(["worktree", "remove", "--force"])
            .arg(&self.path)
            .output();
    }
}
