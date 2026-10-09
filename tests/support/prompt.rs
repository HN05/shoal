use std::{
    io::{Read, Write},
    process::{Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

/// Run `command` with a terminal on stdin and stderr, typing `answer` ahead of
/// its prompts. Returns the exit status and the terminal transcript.
pub fn answer(command: &mut Command, answer: &str) -> (ExitStatus, String) {
    let (mut master, slave) = super::pty::open();
    let mut child = command
        .stdin(slave.try_clone().unwrap())
        .stderr(slave.try_clone().unwrap())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    master.write_all(answer.as_bytes()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut transcript = Vec::new();
    let status = loop {
        let _ = master.read_to_end(&mut transcript);
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "hung: {transcript:?}");
        thread::sleep(Duration::from_millis(10));
    };
    // Keep the slave open while draining: macOS can discard unread data on close.
    let _ = master.read_to_end(&mut transcript);
    drop(slave);
    (status, String::from_utf8_lossy(&transcript).into_owned())
}
