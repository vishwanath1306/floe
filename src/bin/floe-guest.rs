//! Runs inside the guest. Statically linked, so the rootfs needs no libraries
//! for it beyond what busybox and bash already require.
//!
//! Executes the task's verify script, then reports the exit status and output
//! to the host over vsock. Reporting out-of-band rather than on the console
//! means kernel printk cannot interleave with the result, and it works without
//! udev -- `/dev/vsock` comes from devtmpfs, unlike the named virtio-port nodes
//! vng would otherwise use.
//!
//! Never fails the run by itself: if the socket cannot be reached it falls back
//! to printing the sentinel on stdout, which the host also knows how to read.

use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::process::{Command, Stdio};

const USAGE: &str = "usage: floe-guest <vsock-port> <script>";
/// CID 2 is the host, by convention.
const VMADDR_CID_HOST: u32 = 2;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("{USAGE}");
        std::process::exit(2);
    }
    let port: u32 = match args[1].parse() {
        Ok(p) => p,
        Err(_) => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    let script = &args[2];

    let output = Command::new("bash")
        .arg(script)
        .stdin(Stdio::null())
        .output();

    let (exit, text) = match output {
        Ok(out) => {
            let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
            let err = String::from_utf8_lossy(&out.stderr);
            if !err.is_empty() {
                text.push_str(&err);
            }
            // A verify script killed by a signal is a failure, not a pass;
            // map it to a nonzero code rather than letting None become 0.
            (out.status.code().unwrap_or(128), text)
        }
        Err(e) => (127, format!("could not run {script}: {e}\n")),
    };

    // Mirror to the console too: if the vsock send fails this is the only
    // trace, and it makes `console.log` readable on its own.
    print!("{text}");
    let _ = std::io::stdout().flush();

    let payload = format!(
        "{{\"exit\":{},\"output\":{}}}",
        exit,
        json_string(&text)
    );

    if let Err(e) = send(port, &payload) {
        eprintln!("floe-guest: vsock report failed: {e}");
        println!("FLOE_EXIT={exit}");
        let _ = std::io::stdout().flush();
    }
}

fn send(port: u32, payload: &str) -> std::io::Result<()> {
    // SAFETY: plain socket syscalls; the fd is adopted by OwnedFd so it closes
    // exactly once, and the host sees EOF as end-of-report.
    let raw = unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM, 0) };
    if raw < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };

    let mut addr: libc::sockaddr_vm = unsafe { std::mem::zeroed() };
    addr.svm_family = libc::AF_VSOCK as libc::sa_family_t;
    addr.svm_cid = VMADDR_CID_HOST;
    addr.svm_port = port;

    let rc = unsafe {
        libc::connect(
            fd.as_raw_fd(),
            &addr as *const _ as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t,
        )
    };
    if rc < 0 {
        return Err(std::io::Error::last_os_error());
    }

    let mut file = std::fs::File::from(fd);
    file.write_all(payload.as_bytes())?;
    file.flush()
}

/// Minimal JSON string escaping. Hand-rolled to keep the guest binary free of
/// dependencies; the guest ships inside the rootfs and should stay small.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::json_string;

    #[test]
    fn escapes_what_json_requires() {
        assert_eq!(json_string("a\"b"), r#""a\"b""#);
        assert_eq!(json_string("a\\b"), r#""a\\b""#);
        assert_eq!(json_string("line\n"), r#""line\n""#);
    }

    #[test]
    fn escapes_control_characters() {
        // Kernel output can carry stray control bytes; unescaped they would
        // produce a payload the host cannot parse.
        assert_eq!(json_string("\u{1}"), "\"\\u0001\"");
    }

    #[test]
    fn leaves_ordinary_text_alone() {
        assert_eq!(json_string("OK: floe_probe=42"), r#""OK: floe_probe=42""#);
    }
}
