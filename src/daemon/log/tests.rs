use std::io::{self, Write};

struct Full;

impl Write for Full {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::from_raw_os_error(libc::ENOSPC))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn lines_are_timestamped_and_failed_writes_are_dropped() {
    let mut out = Vec::new();
    super::line(&mut out, format_args!("auto cleanup: {}", "failed"));
    let text = String::from_utf8(out).unwrap();
    assert!(text.ends_with(" auto cleanup: failed\n"), "{text}");
    assert_eq!(text.find(' '), Some(10), "{text}");
    super::line(&mut Full, format_args!("disk full"));
}
