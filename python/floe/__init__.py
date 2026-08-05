"""floe - a VM-backed harness for Linux kernel agent tasks.

The engine is Rust (``floe._core``): worktree lifecycle, process supervision,
the VMM boundary, console capture, and grading. This layer is configuration and
presentation only.
"""

from __future__ import annotations

import os
from pathlib import Path
from typing import Any

from ._core import __version__, check_tools, run_trial as _run_trial

__all__ = ["run", "check_tools", "paths", "find_tasks", "__version__"]


def paths(root: Path | str | None = None) -> dict[str, Path]:
    """Resolve the three directories a trial needs.

    Environment variables win, so a caller can point at another kernel tree or
    share one ccache across checkouts. Mirrors the Rust binary's defaults.
    """
    base = Path(root) if root is not None else Path.cwd()
    return {
        "kernel_src": Path(os.environ.get("FLOE_KERNEL_SRC", base / "kernel-src")),
        "runs_dir": Path(os.environ.get("FLOE_RUNS_DIR", base / "runs")),
        "ccache_dir": Path(os.environ.get("FLOE_CCACHE_DIR", base / ".ccache")),
        "rootfs": Path(os.environ.get("FLOE_ROOTFS", base / "rootfs")),
    }


def run(
    task_dir: Path | str,
    agent: str = "claude",
    *,
    root: Path | str | None = None,
    keep: bool = False,
) -> dict[str, Any]:
    """Run one trial and return its result.

    Blocks for as long as the build and boot take, but the Rust side drops the
    GIL, so several trials can run concurrently from threads.

    agent:
        ``claude``    headless Claude Code
        ``solution``  the task's reference patch -- no tokens spent
        ``none``      change nothing; a correct task must not pass this
    """
    p = paths(root)
    return _run_trial(
        Path(task_dir).resolve(),
        agent,
        p["kernel_src"],
        p["runs_dir"],
        p["ccache_dir"],
        p["rootfs"],
        keep,
    )


def find_tasks(tasks_dir: Path | str = "tasks") -> list[Path]:
    """Every directory under *tasks_dir* that looks like a task."""
    root = Path(tasks_dir)
    if not root.is_dir():
        return []
    return sorted(d for d in root.iterdir() if (d / "task.toml").is_file())
