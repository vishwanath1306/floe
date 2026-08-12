# floe

floe runs Linux kernel tasks for coding agents. An agent edits a real kernel
tree; floe builds it, boots it in an ephemeral KVM guest, and scores what
happens.

```
$ floe run tasks/sysctl-probe --agent claude

  outcome   PASS
  reward    1.0
  detail    verify.sh passed inside the agent-built kernel
  evidence  runs/sysctl-probe-claude-<id>
```

## How it works

```
workspace   git worktree off a shared kernel clone; the agent edits here
   ↓ build
artifact    bzImage
   ↓ boot    ← ephemeral guest, minimal rootfs, task dir mounted read-only
guest
   ↓ report  ← a static helper runs verify.sh and answers over vsock
evidence    guest exit status + output + serial console
   ↓ grade
reward      computed on the host
```

Reward is computed host-side from evidence rather than read from a file the
tests write inside the guest. A kernel that panics or hangs never gets far
enough to write anything — but it does produce a console, so those stay
scoreable:

| Outcome | Meaning |
|---|---|
| `PASS` | `verify.sh` succeeded on the agent's kernel |
| `FAIL_TEST` | Kernel booted; the task's own check failed |
| `FAIL_BUILD` / `FAIL_BUILD_TIMEOUT` | Changes did not compile |
| `FAIL_NO_IMAGE` | Build succeeded, no bootable image |
| `FAIL_PANIC` | `Kernel panic - not syncing` on the console |
| `FAIL_OOPS` | Oops, `BUG:`, or `WARNING: CPU:` on the console |
| `FAIL_HANG` | Never finished, and printed no fault |

Console evidence outranks the guest's own result, so a kernel that faults but
still passes `verify.sh` does not pass.

Each run leaves its evidence under `runs/`: the agent's diff, the build log,
the serial console, and `reward.json`.

## Running it

```bash
sudo dnf install -y virtme-ng qemu-system-x86 busybox ccache \
                    elfutils-libelf-devel dwarves glibc-static
git clone https://github.com/torvalds/linux.git kernel-src

./scripts/build-rootfs.sh rootfs    # ~9 MB guest root, once
uv sync                             # builds the Rust core via maturin

uv run floe doctor                  # check host prerequisites
uv run floe tasks
uv run floe run tasks/sysctl-probe --agent solution
```

Needs an accessible `/dev/kvm`; without it QEMU falls back to software
emulation and these tasks become impractically slow. `floe doctor` checks for
it, along with `vng`, `qemu`, the kernel clone and the guest root.

`uv sync` drives maturin, so there is no separate build step. Editing the Rust
core triggers a rebuild; Python under `python/` is installed editable.
`cargo build --release` gives a standalone `floe` binary if you would rather
skip Python.

Full flag reference: [CLI.md](CLI.md).

## Writing a task

```
tasks/<name>/
  task.toml             config, including the base kernel ref
  instruction.md        what the agent is told
  verify.sh             runs INSIDE the guest; exit code is the signal
  setup.patch           optional: hand the agent an already-broken tree
  solution/solve.patch  the oracle
```

`verify.sh` never writes a reward file — it exits, and the host grades. It
should assert it is running on the kernel it expects; without that a
misconfigured harness could boot the host kernel and let a no-op agent pass.

Both mutations may be a `.patch` (applied with `git apply`, preferred) or a
`.sh`. A patch has no escaping layer between you and the edit.

Agents: `claude` and `muse` run the respective coding harnesses headless,
`solution` applies the oracle, and `none` changes nothing — a correct task must
not pass `none`.

An agent is only an argv, so adding a harness is a few lines. Since the tree,
the build and the grading are identical across them, the same task compares
harnesses directly.

## Task format

The layout is Harbor-compatible — `task.toml`, `instruction.md`, `solution/` —
so tasks can move between the two. floe adds a `[kernel]` block for what a
kernel task needs:

```toml
[kernel]
base_ref = "v6.12"      # the tree this task is written against
skip_modules = true
guest_cpus = 4
guest_memory = "2G"
```

`base_ref` is per-task, so two tasks can sit on different kernel releases.

## Layout

Rust core, Python surface. The core owns process and VM lifecycle; Python owns
config, CLI and reporting.

```
src/
  proc.rs        process supervision: timeouts, process-group kills
  workspace.rs   per-run git worktree off the shared clone
  vmm.rs         the Vmm trait; virtme-ng is the implementation
  vsock.rs       host side of the guest result channel
  grade.rs       outcome classification from evidence
  trial.rs       the trial spine
  task.rs        task loading
  lib.rs         PyO3 module (feature "python")
  main.rs        standalone binary, same core
  bin/floe-guest.rs   static helper that runs inside the guest
python/floe/     CLI and the importable API
scripts/         guest rootfs builder
tasks/           task definitions
```

One crate builds both a `cdylib` (the Python extension) and a `bin`, so a
broken PyO3 build never blocks running a trial.

## The kernel tree

A sibling clone, not a submodule: the base tree is a per-task property, the
history is gigabytes against a repo that is otherwise tiny, and it is shared
state you may already have elsewhere. Point `FLOE_KERNEL_SRC` at any clone —
each trial takes a worktree from it.
