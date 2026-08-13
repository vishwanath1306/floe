You are working in a Linux kernel source tree (v6.12) at the current directory.

This tree does not boot cleanly. A recent change to it introduced a fault that
shows up during boot. Find it and fix it.

A correct kernel:

- reaches userspace with no oops, BUG, or WARNING on the console, and
- logs exactly this line during boot:

  ```
  floe: selftest ok, version 1
  ```

Only edit files in this tree. Do not modify the build system or the kernel
config.
