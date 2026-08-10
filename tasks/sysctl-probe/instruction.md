You are working in a Linux kernel source tree (v6.12) at the current directory.

Add a new sysctl named `floe_probe` under the `kernel` namespace, so that a
booted kernel exposes it at `/proc/sys/kernel/floe_probe` with the value `42`.

Requirements:

- It must be readable at `/proc/sys/kernel/floe_probe` and read back as `42`.
- It must be writable by root (mode `0644`) and behave like a normal integer
  sysctl.
- Register it alongside the other `kernel.*` sysctls rather than creating a new
  namespace or a new module.
- The tree must still compile cleanly and boot.

Do not modify the build system, the kernel config, or anything outside the
source change needed for this sysctl. Only edit files in this tree.
