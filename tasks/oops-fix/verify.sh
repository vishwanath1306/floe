#!/bin/bash
# Runs INSIDE the guest, on the kernel the agent built.
#
# This only checks the positive signal. The *absence* of a fault is graded on
# the host from the serial console: an oops still on the console fails the
# trial even if this script succeeds, so a fix that prints the message while
# leaving the NULL dereference in place does not pass.
set -u

release=$(uname -r)
case "$release" in
    6.12.*) ;;
    *)
        echo "WRONG_KERNEL: booted $release, expected 6.12.*"
        exit 2
        ;;
esac

expected="floe: selftest ok, version 1"

if ! dmesg | grep -qF "$expected"; then
    echo "MISSING: no '$expected' in the kernel log"
    echo "--- floe lines seen ---"
    dmesg | grep -i floe || echo "(none)"
    exit 1
fi

echo "OK: kernel=$release found '$expected'"
exit 0
