#!/bin/bash
# Runs INSIDE the guest, on the kernel the agent built.
#
# Exit code is the signal: floe grades on the host from this code plus the
# serial console, so this script never writes a reward file. If the kernel
# panics before reaching here, the console is what gets graded instead.
set -u

# Guard against the harness silently booting the host kernel instead of the
# agent's build -- that would let a no-op agent pass. The task tree is v6.12;
# the host runs 6.16.x.
release=$(uname -r)
case "$release" in
    6.12.*) ;;
    *)
        echo "WRONG_KERNEL: booted $release, expected 6.12.*"
        exit 2
        ;;
esac

probe=/proc/sys/kernel/floe_probe

if [ ! -e "$probe" ]; then
    echo "MISSING: $probe does not exist on kernel $release"
    exit 1
fi

value=$(cat "$probe" 2>/dev/null) || {
    echo "UNREADABLE: could not read $probe"
    exit 1
}

if [ "$value" != "42" ]; then
    echo "BAD_VALUE: $probe reads '$value', expected '42'"
    exit 1
fi

# Mode is part of the ask: a read-only sysctl would satisfy the read check
# while missing half the requirement.
mode=$(stat -c '%a' "$probe" 2>/dev/null || echo "?")
if [ "$mode" != "644" ]; then
    echo "BAD_MODE: $probe has mode $mode, expected 644"
    exit 1
fi

echo "OK: kernel=$release floe_probe=$value mode=$mode"
exit 0
