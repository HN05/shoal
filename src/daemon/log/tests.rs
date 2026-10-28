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

#[test]
fn durations_show_their_two_largest_units() {
    use std::time::Duration;
    assert_eq!(super::elapsed(Duration::from_secs(45)), "45s");
    assert_eq!(super::elapsed(Duration::from_secs(125)), "2m 5s");
    assert_eq!(super::elapsed(Duration::from_secs(7_380)), "2h 3m");
}

#[tokio::test(start_paused = true)]
async fn slow_work_still_returns_its_output() {
    let slow = async {
        tokio::time::sleep(super::SLOW * 2).await;
        7
    };
    assert_eq!(super::when_slow("request status", slow).await, 7);
}
