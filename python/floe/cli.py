"""Command line front end. All the work happens in the Rust core."""

from __future__ import annotations

import argparse
import json
import sys
import tomllib
from pathlib import Path

import floe

# Every non-PASS outcome is worth distinguishing at a glance -- the whole point
# of grading host-side is that "panicked" and "test failed" are different news.
GLYPH = {
    "PASS": "PASS   ",
    "FAILTEST": "FAIL   ",
    "FAILSTYLE": "STYLE  ",
    "FAILBUILD": "BUILD  ",
    "FAILBUILDTIMEOUT": "BUILD !",
    "FAILNOIMAGE": "NOIMAGE",
    "FAILPANIC": "PANIC  ",
    "FAILOOPS": "OOPS   ",
    "FAILHANG": "HANG   ",
}


def _report(result: dict) -> None:
    outcome = result["outcome"]
    print()
    print(f"  outcome   {GLYPH.get(outcome, '?')}  {outcome}")
    print(f"  reward    {result['reward']}")
    print(f"  detail    {result['detail']}")
    print(f"  diff      {result['diff_stat'] or '(agent changed nothing)'}")
    print(
        "  timing    agent {agent_secs:.0f}s  build {build_secs:.0f}s  "
        "boot {boot_secs:.0f}s".format(**result)
    )
    st = result.get("style") or {}
    if st.get("ran"):
        print(f"  style     checkpatch: {st['errors']} errors, {st['warnings']} warnings")
    elif st.get("skipped"):
        print(f"  style     not checked ({st['skipped']})")
    print(f"  evidence  {result['run_dir']}")


def _cmd_run(args: argparse.Namespace) -> int:
    try:
        result = floe.run(args.task, agent=args.agent, keep=args.keep)
    except RuntimeError as exc:
        # The harness could not run the trial at all. Distinct from a trial
        # that ran and failed, which is exit 1 -- a panicking kernel is a
        # result, a missing task directory is not.
        print(f"floe: {exc}", file=sys.stderr)
        return 2
    if args.json:
        json.dump(result, sys.stdout, indent=2)
        print()
    else:
        _report(result)
    return 0 if result["reward"] > 0 else 1


def _cmd_tasks(args: argparse.Namespace) -> int:
    found = floe.find_tasks(args.dir)
    if not found:
        print(f"no tasks under {args.dir}/", file=sys.stderr)
        return 1
    for task_dir in found:
        spec = tomllib.loads((task_dir / "task.toml").read_text())
        name = spec.get("task", {}).get("name", task_dir.name)
        base = spec.get("kernel", {}).get("base_ref", "HEAD")
        sol = task_dir / "solution"
        oracle = next(
            (k for k, f in (("patch", "solve.patch"), ("script", "solve.sh"))
             if (sol / f).is_file()),
            "-",
        )
        print(f"{task_dir.name:24} {name:28} {base:10} {oracle}")
    return 0


def _cmd_doctor(_: argparse.Namespace) -> int:
    """Check the host can run a trial at all, before one is attempted."""
    problems = []
    try:
        floe.check_tools()
    except RuntimeError as exc:
        problems.append(str(exc))

    if not Path("/dev/kvm").exists():
        problems.append("/dev/kvm missing -- trials would fall back to TCG emulation")

    p = floe.paths()
    if not (p["kernel_src"] / ".git").is_dir():
        problems.append(f"no kernel git clone at {p['kernel_src']}")

    if not (p["rootfs"] / "bin" / "busybox").is_file():
        problems.append(
            f"no guest rootfs at {p['rootfs']} -- run scripts/build-rootfs.sh {p['rootfs']}"
        )

    for problem in problems:
        print(f"  FAIL  {problem}")
    if problems:
        return 1
    print(f"  OK    floe {floe.__version__}, kernel tree at {p['kernel_src']}")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="floe", description=__doc__)
    sub = parser.add_subparsers(dest="cmd", required=True)

    run_p = sub.add_parser("run", help="run one trial")
    run_p.add_argument("task", type=Path, help="path to a task directory")
    run_p.add_argument(
        "--agent",
        default="claude",
        choices=["claude", "muse", "solution", "oracle", "none", "nop"],
        metavar="{claude,muse,solution,none}",
        help="what mutates the workspace (default: claude); "
        "solution=oracle, none=nop",
    )
    run_p.add_argument(
        "--keep", action="store_true", help="keep the run's worktree for inspection"
    )
    run_p.add_argument(
        "--json", action="store_true", help="print the result as JSON only"
    )
    run_p.set_defaults(func=_cmd_run)

    tasks_p = sub.add_parser("tasks", help="list available tasks")
    tasks_p.add_argument(
        "--dir", default="tasks", help="directory to scan for tasks (default: tasks)"
    )
    tasks_p.set_defaults(func=_cmd_tasks)

    doctor_p = sub.add_parser("doctor", help="check host prerequisites")
    doctor_p.set_defaults(func=_cmd_doctor)

    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
