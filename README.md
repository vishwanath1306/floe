# floe

A minimal VM-backed harness for Linux kernel agent tasks: **virtme-ng + Claude**.

The agent edits a real kernel tree, the harness builds it, boots the result in
a KVM guest, and grades what happens.

Measured on a 368-core devserver, excluding agent time: **~59 s per iteration**
(54 s build + 5 s boot) cold, **~45 s** warm (40 s + 5 s). With ccache tuned for
cross-worktree reuse the hit rate is 50%; the residual build time is link,
objtool and modpost, which ccache cannot help. The design doc's concern was
"5 min → hours/iteration" under TCG emulation — with KVM this is a minute.

```
$ floe run tasks/sysctl-probe --agent claude

  outcome   PASS      PASS
  reward    1.0
  detail    verify.sh passed inside the agent-built kernel
  diff      1 file(s), +9/-0
  timing    agent 41s  build 54s  boot 5s
  evidence  runs/sysctl-probe-claude-1786567182
```

## Why not just add an environment to Harbor

Harbor's `BaseEnvironment` core (`start`/`stop`/`exec`/`upload`/`download`) is
transport-agnostic, and a guest satisfies it. The mismatch is elsewhere:

1. **Harbor's only "VM" environment isn't one.** `environments/ec2.py` is
   `ComposeServiceOpsMixin` + `DinDComposeOps`; it bootstraps Docker on the
   instance and runs compose. Same for `skypilot` and `gke`. There is no VM
   abstraction to extend — the slot is occupied by containers-somewhere-else.

2. **One long-lived environment vs. N ephemeral boots.** Harbor runs
   `start → agent.run → verify → stop` against a single sandbox, with the
   verifier either `SHARED` (same container) or `SEPARATE` (a second container
   built from a Dockerfile). A kernel task fits neither: the agent's workspace
   and the machine under test are different machines, and the machine under
   test **does not exist until the agent builds it**.

3. **Boot failure is a graded outcome, not an infrastructure error.** Harbor's
   reward is a file the tests write *inside* the environment
   (`verifier/verifier.py:227`). If the agent's kernel panics there is no
   inside — nothing runs, nothing writes, and the trial looks like broken
   infrastructure. "Panicked on boot with this oops" is often the most
   informative result a kernel task can produce.

4. **Environment identity is a Dockerfile hash** (`environment_content_hash`).
   A kernel task's identity is `(base tree, kconfig, toolchain)`, and its
   expensive shared state is the build cache.

5. **The capability vocabulary is containers all the way down** — "allocate
   GPUs to containers", "run Windows containers", `docker_compose`. Nothing
   expresses "can boot a modified guest kernel".

So the task *format* stays Harbor-compatible and the verbs stay `exec`-shaped,
but the trial loop is different by necessity.

## The inversion that matters

```
workspace   host git worktree; the agent edits here
   ↓ build
artifact    bzImage
   ↓ boot    ← fallible, first-class, serial console captured
guest       ephemeral, N per trial
   ↓ exec
evidence    guest exit status + console
   ↓ grade
reward      computed on the HOST from evidence
```

Reward is computed host-side from evidence rather than written guest-side into
a file. That single change is what makes panics, hangs, and boot regressions
gradeable instead of infrastructure exceptions:

| Outcome | Means |
|---|---|
| `PASS` | `verify.sh` succeeded on the agent's kernel |
| `FAIL_TEST` | kernel booted; the task's own check failed |
| `FAIL_BUILD` / `FAIL_BUILD_TIMEOUT` | changes did not compile |
| `FAIL_NO_IMAGE` | build succeeded, no bootable image |
| `FAIL_PANIC` | `Kernel panic - not syncing` on the console |
| `FAIL_OOPS` | oops, `BUG:`, or `WARNING: CPU:` on the console |
| `FAIL_HANG` | never finished, and printed no fault |

Console evidence outranks the timeout: a panic usually *also* hangs, and
reporting that as a hang would hide the cause.

## Layout

Rust core, Python surface. The core owns everything stateful and
failure-sensitive; Python owns config, CLI, and reporting.

```
src/
  proc.rs        process supervision: timeouts, process-group kills
  workspace.rs   per-run git worktree off the shared clone
  vmm.rs         the Vmm trait; Vng is the only implementation today
  grade.rs       outcome classification from evidence
  trial.rs       the trial spine
  task.rs        task.toml / instruction.md loading
  lib.rs         PyO3 module (feature "python")
  main.rs        standalone binary, same core
python/floe/   CLI and the importable API
tasks/           task definitions
```

One crate builds both a `cdylib` (the Python extension) and a `bin`, so a
broken PyO3 build never blocks running a trial. PyO3 sits behind an optional
feature because `extension-module` would break the bin target.

## The VMM boundary

`vng` is **not a VMM** — it is a Python program that emits a
`qemu-system-x86_64` command line. Swapping it means reproducing three things
it gives us free, which is why they are named explicitly in `vmm.rs`:

1. virtio-fs projection of the host filesystem into the guest, so `verify.sh`
   is reachable by host path with no disk image and no upload step;
2. a guest init that runs one command and returns its exit status;
3. kconfig and initramfs generation for an arbitrary tree.

**Cloud Hypervisor** is viable — it supports vhost-user-fs via the same
`virtiofsd` — but costs device breadth (virtio-only rules out PCI/USB/NVMe
driver tasks) and gdbstub maturity. **Firecracker is the wrong tool**: no
virtio-fs at all, so every trial needs a rootfs image. Note that the usual
reason to leave QEMU — snapshot/restore to skip boot — **does not apply here**,
because when the artifact under test *is* the kernel, every iteration is a
mandatory cold boot.

## Setup

```bash
sudo dnf install -y virtme-ng qemu-system-x86 busybox ccache \
                    elfutils-libelf-devel dwarves glibc-static
git clone https://github.com/torvalds/linux.git kernel-src

./scripts/build-rootfs.sh rootfs       # 9.4 MB guest root, once
uv sync                                # builds the Rust core via maturin

uv run floe doctor
uv run floe tasks
uv run floe run tasks/sysctl-probe --agent solution
```

`uv sync` drives maturin, so there is no separate build step. `[tool.uv]
cache-keys` includes `Cargo.toml` and `src/**/*.rs`, so editing the Rust core
triggers a rebuild -- without that, `uv run` silently keeps a stale extension
module. Python under `python/` is installed editable and needs no resync.

`cargo build --release` still gives a standalone `floe` binary if you would
rather skip Python entirely.

Full flag reference: [CLI.md](CLI.md).

### The kernel tree is a sibling clone, not a submodule

Deliberately. A submodule would pin one kernel commit for the whole repo, but
the base tree is a **per-task** property -- `base_ref` in `task.toml`, so two
tasks can sit on different releases. It would also drag a multi-gigabyte
history into every clone of a repo that is otherwise 600 KB, and the tree is
shared state a user may already have elsewhere. Point `FLOE_KERNEL_SRC` at any
clone; `runs/` takes a worktree from it per trial.

Requires an accessible `/dev/kvm`. Without it QEMU silently falls back to TCG
software emulation, which is 10–20× slower and makes these tasks infeasible —
`floe doctor` checks for this.

Host quirks encoded in `vmm.rs`: `--disable-microvm` is required because this
seabios build ships no `bios-microvm.bin`, and the microvm machine type caps at
288 vCPUs (below this box's 368). `--verbose` is also mandatory -- without it
vng emits no console at all, and the console is the evidence panics are graded
from.

The guest root must contain every mountpoint up front (`/task`, `/dev/pts`,
`/dev/shm`): `/` is read-only in the guest, so a missing `/task` fails the
task-directory mount and kills init.

### Agents

- `claude` — headless Claude Code in the worktree
- `solution` — the task's reference patch; proves the harness works and the
  task is solvable without spending tokens
- `none` — changes nothing; **a correct task must not pass this**

## Writing a task

```
tasks/<name>/
  task.toml             Harbor-compatible, plus a [kernel] block
  instruction.md        what the agent is told
  verify.sh             runs INSIDE the guest; exit code is the signal
  setup.patch           optional: hand the agent an already-broken tree
  solution/solve.patch  the oracle
```

Both mutations may be a `.patch` (applied with `git apply`, preferred) or a
`.sh`. Prefer the patch: a script has to reproduce an edit through string
surgery, which means escaping C inside Python inside a shell heredoc -- three
layers that quietly disagree. That cost us a `stray '\' in program` build
failure. A diff has no escaping layer and reads as the change itself.

`verify.sh` never writes a reward file — it exits, and the host grades. It
should also assert it is running on the *expected* kernel; without that guard a
misconfigured harness could boot the host kernel and let a no-op agent pass.

## Status

Both tasks pass end to end, against their oracles and against a real agent:

```
                 oracle                     claude
sysctl-probe     PASS  build 40s boot 4s    PASS  agent  73s  build 44s  boot 4s
oops-fix         PASS  build 44s boot 4s    PASS  agent 251s  build 40s  boot 4s
```

On `oops-fix` the agent never sees the console -- the agent phase runs before
the boot -- so it has to find the NULL dereference by reading `init/main.c`.
It cannot shortcut by printing the expected message either: console signatures
are checked before the guest's own result, so leaving the dereference in place
scores `FAIL_OOPS` even when `verify.sh` succeeds.

Working: worktree isolation, minimal guest root, vsock result reporting, build,
boot, host-side grading, both CLIs, patch-based oracles, 25 unit tests over the
grading and channel semantics.

Not done yet:
- **Concurrency.** vng builds with `-j $(nproc)`, so parallel trials would
  thrash. Bounding it means driving `make` directly instead of `vng --build`.
- **A warm build cache still costs 40 s.** Going below that means keeping a
  populated `O=` build directory per task baseline and rsyncing it into each
  worktree, so only changed objects rebuild.
- **No sandboxing of the agent.** It runs as you, on the host, with your
  filesystem projected into the guest. The design doc's brokered-`/dev/kvm`,
  unprivileged-agent model is the real answer and none of it is here.
- Only `FAIL_*` granularity; no partial credit.
- One task, one VMM backend, x86 only.
