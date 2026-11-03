//! This machine's identity, which tells an import whether it runs where the
//! export was made, and its name.

/// The machine's hardware or installation ID; `None` when it has none.
pub fn id() -> Option<String> {
    platform_id().filter(|id| !id.is_empty())
}

/// The machine's host name, or `unknown` when it cannot be read.
pub fn hostname() -> String {
    let mut buffer = [0u8; 256];
    // SAFETY: gethostname writes at most `len` bytes into the provided buffer.
    let result = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
    if result != 0 {
        return "unknown".into();
    }
    let end = buffer.iter().position(|b| *b == 0).unwrap_or(buffer.len());
    String::from_utf8_lossy(&buffer[..end]).into_owned()
}

#[cfg(target_os = "macos")]
fn platform_id() -> Option<String> {
    let mut uuid = [0u8; 16];
    let timeout = libc::timespec {
        tv_sec: 1,
        tv_nsec: 0,
    };
    // SAFETY: gethostuuid writes the 16 bytes of a UUID into `uuid`.
    let result = unsafe { libc::gethostuuid(uuid.as_mut_ptr(), &timeout) };
    (result == 0).then(|| uuid.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(target_os = "linux")]
fn platform_id() -> Option<String> {
    ["/etc/machine-id", "/var/lib/dbus/machine-id"]
        .into_iter()
        .find_map(|path| std::fs::read_to_string(path).ok())
        .map(|id| id.trim().to_owned())
}
