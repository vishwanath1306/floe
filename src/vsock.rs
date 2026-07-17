//! Host side of the guest result channel.
//!
//! The guest reports over `AF_VSOCK` rather than the serial console. Console
//! scraping needs a sentinel and tolerates interleaved printk; vng's own
//! `/dev/virtio-ports/virtme.ret` needs udev to materialise the named device
//! node, which a minimal rootfs does not have. vsock needs neither: the guest
//! opens a socket to CID 2 and writes a JSON line.
//!
//! The channel is also out-of-band, so a guest that is spewing kernel messages
//! cannot corrupt its own result.

use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// What the guest sends back.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuestReport {
    /// verify.sh's exit status inside the guest.
    pub exit: i32,
    /// Whatever verify.sh printed, captured in the guest rather than scraped
    /// off a console shared with the kernel.
    #[serde(default)]
    pub output: String,
}

pub struct Listener {
    fd: OwnedFd,
    pub port: u32,
    pub guest_cid: u32,
}

impl Listener {
    /// Bind a vsock listener, searching upward from `port_hint` so concurrent
    /// trials do not collide.
    pub fn bind(port_hint: u32) -> Result<Self> {
        for offset in 0..64u32 {
            let port = port_hint + offset;
            match Self::bind_exact(port) {
                Ok(l) => return Ok(l),
                Err(_) if offset < 63 => continue,
                Err(e) => return Err(e),
            }
        }
        bail!("no free vsock port near {port_hint}")
    }

    fn bind_exact(port: u32) -> Result<Self> {
        // SAFETY: plain socket syscalls; the fd is adopted by OwnedFd below so
        // it is closed exactly once.
        let raw = unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM, 0) };
        if raw < 0 {
            return Err(std::io::Error::last_os_error()).context("socket(AF_VSOCK)");
        }
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };

        let mut addr: libc::sockaddr_vm = unsafe { std::mem::zeroed() };
        addr.svm_family = libc::AF_VSOCK as libc::sa_family_t;
        addr.svm_cid = libc::VMADDR_CID_ANY;
        addr.svm_port = port;

        let rc = unsafe {
            libc::bind(
                fd.as_raw_fd(),
                &addr as *const _ as *const libc::sockaddr,
                std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t,
            )
        };
        if rc < 0 {
            return Err(std::io::Error::last_os_error()).context("bind");
        }
        if unsafe { libc::listen(fd.as_raw_fd(), 1) } < 0 {
            return Err(std::io::Error::last_os_error()).context("listen");
        }

        Ok(Self {
            fd,
            port,
            // Guest CIDs must be unique among running VMs. Deriving from the
            // port -- itself already collision-checked by bind -- keeps the two
            // allocations consistent without a second registry.
            guest_cid: cid_for_port(port),
        })
    }

    /// Wait for the guest to report. Returns None if nothing arrives.
    ///
    /// `abandoned` lets the caller cut the wait short once the VM has exited.
    /// Without it a guest that dies on boot would still cost the full verify
    /// timeout, since no connection is ever coming.
    pub fn recv(&self, timeout: Duration, abandoned: &AtomicBool) -> Result<Option<GuestReport>> {
        let deadline = Instant::now() + timeout;
        let stream = match self.accept_before(deadline, abandoned)? {
            Some(s) => s,
            None => return Ok(None),
        };

        let mut raw = String::new();
        let mut stream = std::fs::File::from(stream);
        stream
            .read_to_string(&mut raw)
            .context("reading guest report")?;
        if raw.trim().is_empty() {
            return Ok(None);
        }
        let report = serde_json::from_str(raw.trim())
            .with_context(|| format!("parsing guest report: {raw:?}"))?;
        Ok(Some(report))
    }

    fn accept_before(&self, deadline: Instant, abandoned: &AtomicBool) -> Result<Option<OwnedFd>> {
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() || abandoned.load(Ordering::Relaxed) {
                return Ok(None);
            }
            let mut pfd = libc::pollfd {
                fd: self.fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let ms = remaining.as_millis().min(1000) as libc::c_int;
            let rc = unsafe { libc::poll(&mut pfd, 1, ms) };
            if rc < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err).context("poll");
            }
            if rc == 0 {
                continue; // re-check the deadline
            }
            let raw = unsafe { libc::accept(self.fd.as_raw_fd(), std::ptr::null_mut(), std::ptr::null_mut()) };
            if raw < 0 {
                return Err(std::io::Error::last_os_error()).context("accept");
            }
            return Ok(Some(unsafe { OwnedFd::from_raw_fd(raw) }));
        }
    }
}

/// CIDs 0-2 are reserved (hypervisor, local, host), so guests start at 3.
fn cid_for_port(port: u32) -> u32 {
    3 + (port % 1000)
}

/// A starting port that differs between concurrent processes.
pub fn default_port_hint() -> u32 {
    9000 + (std::process::id() % 500)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guest_cids_avoid_reserved_range() {
        // 0=hypervisor, 1=local, 2=host. Handing a guest one of those would
        // make the connection land somewhere unexpected rather than fail.
        for port in [0, 1, 2, 999, 9000, 65535] {
            assert!(cid_for_port(port) >= 3, "port {port}");
        }
    }

    #[test]
    fn port_hint_is_in_the_unprivileged_range() {
        let p = default_port_hint();
        assert!((9000..9500).contains(&p));
    }

    #[test]
    fn report_round_trips() {
        let json = r#"{"exit":1,"output":"BAD_VALUE\n"}"#;
        let r: GuestReport = serde_json::from_str(json).unwrap();
        assert_eq!(r.exit, 1);
        assert_eq!(r.output, "BAD_VALUE\n");
    }

    #[test]
    fn report_tolerates_missing_output() {
        let r: GuestReport = serde_json::from_str(r#"{"exit":0}"#).unwrap();
        assert_eq!(r.exit, 0);
        assert!(r.output.is_empty());
    }

    #[test]
    fn bind_then_timeout_yields_no_report() {
        // Nothing connects, so recv must report absence rather than block or
        // invent a result.
        let l = Listener::bind(default_port_hint()).expect("bind vsock");
        assert_eq!(l.guest_cid, cid_for_port(l.port));
        let got = l.recv(Duration::from_millis(150), &AtomicBool::new(false)).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn abandoning_returns_immediately() {
        // A guest that panicked on boot will never connect. Waiting out the
        // full verify timeout for it wastes minutes per failed trial.
        let l = Listener::bind(default_port_hint() + 100).expect("bind vsock");
        let start = Instant::now();
        let got = l.recv(Duration::from_secs(300), &AtomicBool::new(true)).unwrap();
        assert!(got.is_none());
        assert!(start.elapsed() < Duration::from_secs(2), "waited too long");
    }
}
