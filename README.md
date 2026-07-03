# floe

Notes toward a harness for Linux kernel agent tasks.

## The problem

Harbor cannot host kernel-domain tasks: agent environments have no access to a
real, modifiable kernel. Anything that needs one -- eBPF, modules, syscall
behaviour, kernel build and boot -- cannot be authored at all.

## Why not just add an environment to Harbor

Read through `src/harbor/environments/`. The `BaseEnvironment` core
(`start`/`stop`/`exec`/`upload`/`download`) is transport-agnostic and a guest
would satisfy it. The mismatch is elsewhere:

1. Harbor's only "VM" environment is not one. `ec2.py` is
   `ComposeServiceOpsMixin` + `DinDComposeOps` -- it bootstraps Docker on the
   instance and runs compose. Same for skypilot and gke. There is no VM
   abstraction to extend.

2. One long-lived environment vs. N ephemeral boots. Harbor runs
   `start -> agent.run -> verify -> stop` against a single sandbox. A kernel
   task fits neither shape: the agent's workspace and the machine under test
   are different machines, and the machine under test does not exist until the
   agent builds it.

3. Boot failure is a graded outcome, not an infrastructure error. Harbor's
   reward is a file the tests write *inside* the environment. If the agent's
   kernel panics there is no inside.

4. Environment identity is a Dockerfile hash. A kernel task's identity is
   (base tree, kconfig, toolchain).

## Shape to build

    workspace -> build -> artifact -> boot -> guest -> evidence -> reward

Reward computed on the host from evidence, not written guest-side into a file.
That is the change that makes panics and hangs gradeable.
