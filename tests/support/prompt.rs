use std::{
    io::{Read, Write},
    os::unix::process::CommandExt,
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug)]
pub struct AnswerOptions {
    pub controlling_terminal: bool,
    pub kill_on_timeout: bool,
}

impl Default for AnswerOptions {
    fn default() -> Self {
        Self {
            controlling_terminal: false,
            kill_on_timeout: true,
        }
    }
}

/// Run `command` with a terminal on stdin and stderr, typing `answer` ahead of
/// its prompts. Returns its output and the terminal transcript.
pub fn answer(command: &mut Command, answer: &str, options: AnswerOptions) -> (Output, String) {
    if options.controlling_terminal {
        // A tracked command needs a controlling terminal to transfer foreground
        // ownership to its child, not just file descriptors that pass isatty.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0
                    || libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as _, 0) < 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let (mut master, slave) = super::pty::open();
    let mut child = command
        .stdin(slave.try_clone().unwrap())
        .stderr(slave.try_clone().unwrap())
        .stdout(Stdio::piped())
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
        if Instant::now() >= deadline {
            if options.kill_on_timeout {
                let _ = child.kill();
                let _ = child.wait();
            }
            panic!("hung: {transcript:?}");
        }
        thread::sleep(Duration::from_millis(10));
    };
    // Keep the slave open while draining: macOS can discard unread data on close.
    let _ = master.read_to_end(&mut transcript);
    drop(slave);
    let output = child.wait_with_output().unwrap();
    debug_assert_eq!(output.status, status);
    (output, String::from_utf8_lossy(&transcript).into_owned())
}
