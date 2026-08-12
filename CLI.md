# floe CLI

Two front ends over the same Rust core. The Python CLI is the normal one; the
standalone binary exists so a trial can be run without a Python build step.

```
uv run floe <command> [args]      # python
./target/release/floe <task-dir>  # standalone, cargo build --release
```

## floe run

Run one trial: mutate a worktree, build the kernel, boot it, grade it.

```
floe run <task-dir> [--agent claude|muse|solution|none] [--keep] [--json]
```

| Argument | Default | Meaning |
|---|---|---|
| `<task-dir>` | *required* | Path to a task directory (one containing `task.toml`) |
| `--agent` | `claude` | What mutates the workspace — see below |
| `--keep` | off | Keep the run's git worktree instead of removing it |
| `--json` | off | Print the result as JSON only, no human summary |

**`--agent` values**

| Value | Does |
|---|---|
| `claude` | Headless Claude Code (`claude -p`) in the worktree |
| `muse` | Headless Muse Code (`muse exec`) in the worktree |
| `solution` | Applies the task's `solution/solve.patch` (or `solve.sh`). No tokens spent. Aliases: `oracle` |
| `none` | Changes nothing. A correct task must **not** pass this. Aliases: `nop` |

## floe tasks

List tasks, showing name, base kernel ref, and whether an oracle exists.

```
floe tasks [--dir DIR]
```

| Argument | Default | Meaning |
|---|---|---|
| `--dir` | `tasks` | Directory to scan |

## floe doctor

Check host prerequisites before running anything: `vng` / `git` /
`qemu-system-x86_64` on `PATH`, `/dev/kvm` present, a kernel clone, and a built
guest rootfs. Takes no arguments.

```
floe doctor
```

Without `/dev/kvm` QEMU silently falls back to TCG software emulation, which is
10–20× slower and makes these tasks infeasible — hence the check.

## Environment

All optional; each overrides a default derived from the current directory.

| Variable | Default | Meaning |
|---|---|---|
| `FLOE_KERNEL_SRC` | `./kernel-src` | Kernel git clone. A worktree is taken from it per trial |
| `FLOE_RUNS_DIR` | `./runs` | Where run evidence is written |
| `FLOE_CCACHE_DIR` | `./.ccache` | Shared compiler cache across runs |
| `FLOE_ROOTFS` | `./rootfs` | Minimal guest root, built by `scripts/build-rootfs.sh` |

## Exit status

| Code | Meaning |
|---|---|
| `0` | Trial passed (reward > 0) |
| `1` | Trial did not pass — any `FAIL_*` outcome |
| `2` | Usage error, or the harness could not run the trial at all |

A non-passing trial is **1**, not an error: a kernel that panics is a result.
`2` is reserved for the harness itself failing.

## Run output

Each run writes a directory under `FLOE_RUNS_DIR`:

```
runs/<task>-<agent>-<epoch>/
  reward.json    outcome, reward, timings, detail
  agent.diff     what the agent changed
  agent.log      agent stdout/stderr
  setup.log      only if the task has a setup patch/script
  build.log      kernel build output
  console.log    guest serial console -- what panics are graded from
  verify.out     what verify.sh printed inside the guest
  workspace/     the worktree, only with --keep
```

## Outcomes

`reward.json` carries one of:

| Outcome | Meaning |
|---|---|
| `PASS` | `verify.sh` succeeded on the agent's kernel |
| `FAIL_TEST` | Kernel booted; the task's own check failed |
| `FAIL_BUILD` | Changes did not compile |
| `FAIL_BUILD_TIMEOUT` | Build exceeded its deadline |
| `FAIL_NO_IMAGE` | Build succeeded but produced no bzImage |
| `FAIL_PANIC` | `Kernel panic - not syncing` on the console |
| `FAIL_OOPS` | Oops, `BUG:`, or `WARNING: CPU:` on the console |
| `FAIL_HANG` | Never finished, and printed no fault |

Console evidence outranks the guest's own result: a kernel that oopses but
still passes `verify.sh` scores `FAIL_OOPS`, not `PASS`.

## Timeouts

Per-task, set in `task.toml`, not on the command line:

```toml
[agent]    timeout_sec = 1800
[build]    timeout_sec = 3600
[verifier] timeout_sec = 300
```
