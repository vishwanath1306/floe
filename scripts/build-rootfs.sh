#!/bin/bash
# Build the minimal guest rootfs.
#
# Trials must not run against the host filesystem. Projecting `/` into the
# guest would expose the operator's home directory and credentials to agent
# code, and would make results depend on whatever happens to be installed on
# the box. This builds a small, self-contained root instead: busybox for the
# userland, bash because virtme's guest init is a bash script, and nothing else.
#
# Output is a directory, not an image -- vng bind-mounts it as the guest root.
set -euo pipefail

OUT=${1:-rootfs}
BUSYBOX=${BUSYBOX:-/usr/sbin/busybox}

[ -x "$BUSYBOX" ] || { echo "no busybox at $BUSYBOX" >&2; exit 1; }

rm -rf "$OUT"
mkdir -p "$OUT"/{bin,sbin,etc,proc,sys,dev,tmp,run,root,var/log,var/tmp,usr,lib,lib64}
# vng writes an rw overlay over each of these at boot; a missing mountpoint
# fails the overlay and the guest never reaches init.
mkdir -p "$OUT"/{home,opt,srv}
# The guest root is read-only, so every mountpoint has to exist up front:
# /task is where the task directory is mounted, and virtme-init mounts devpts
# and shm. A missing /task kills init outright.
mkdir -p "$OUT"/task "$OUT"/dev/{pts,shm}
chmod 1777 "$OUT/tmp" "$OUT/var/tmp"

# usr-merge. Not cosmetic: bash's compiled-in default PATH is /usr/bin, and the
# kernel cmdline runs the early init shell with no PATH of its own. With /bin
# and /usr/bin as separate directories, that shell cannot find `mount` or
# `mkdir`, the 9p guesttools mount never happens, and the guest panics with
# "Attempted to kill init".
ln -sf ../bin "$OUT/usr/bin"
ln -sf ../sbin "$OUT/usr/sbin"

# --- busybox: one static binary, ~400 applets --------------------------------
cp "$BUSYBOX" "$OUT/bin/busybox"
for applet in $("$BUSYBOX" --list); do
    ln -sf /bin/busybox "$OUT/bin/$applet"
done

# --- bash and its shared library closure -------------------------------------
# virtme-init is #!/bin/bash and uses bashisms, so busybox ash will not do.
copy_with_libs() {
    local binary=$1 dest=$2
    install -D "$binary" "$OUT/$dest"
    # Resolve the closure; the loader itself shows up here too.
    ldd "$binary" 2>/dev/null | grep -oE '/[^ ]+\.so[^ ]*' | sort -u | while read -r lib; do
        [ -e "$lib" ] || continue
        install -D "$lib" "$OUT${lib}"
    done
}

for prog in /bin/bash; do
    copy_with_libs "$prog" "${prog#/}"
done
ln -sf /bin/bash "$OUT/bin/sh"

# --- identity ----------------------------------------------------------------
# Enough for `whoami` and anything that resolves uid 0; no real user database.
printf 'root:x:0:0:root:/root:/bin/bash\n' > "$OUT/etc/passwd"
printf 'root:x:0:\n' > "$OUT/etc/group"
printf 'floe-guest\n' > "$OUT/etc/hostname"
: > "$OUT/etc/fstab"

# --- guest helper -------------------------------------------------------------
# Statically linked so the rootfs needs no extra libraries for it. It runs the
# task's verify script and reports the result to the host over vsock.
if [ -z "${GUEST_BIN:-}" ]; then
    REPO=$(cd "$(dirname "$0")/.." && pwd)
    # An explicit --target keeps RUSTFLAGS off host artifacts; without it the
    # static flag reaches proc-macro crates, which cannot be built that way.
    TRIPLE=$(rustc -vV | sed -n 's/^host: //p')
    RUSTFLAGS="-C target-feature=+crt-static" cargo build --release \
        --manifest-path "$REPO/Cargo.toml" --bin floe-guest \
        --target "$TRIPLE" --target-dir "$REPO/target/guest" >&2
    GUEST_BIN="$REPO/target/guest/$TRIPLE/release/floe-guest"
fi
install -D "$GUEST_BIN" "$OUT/bin/floe-guest"

# The guest init is NOT installed here: vng exports its own over a 9p share
# mounted at /run/virtme/guesttools and execs it from there. The rootfs only has
# to be able to perform that mount, which is why busybox and bash must be
# reachable on the default PATH.

echo "rootfs: $OUT ($(du -sh "$OUT" | cut -f1))"
