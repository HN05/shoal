use std::{
    fs::File,
    io::Read,
    os::fd::{AsRawFd, FromRawFd},
    process::{Command, Output, Stdio},
};

// Run short, offline commands with either output stream attached to a terminal.
fn run(args: &[&str], terminal: Option<bool>, env: &[(&str, &str)]) -> (Output, String) {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_shoal"));
    command
        .args(["--state-dir", root.path().to_str().unwrap()])
        .args(args)
        .env_remove("SHOAL_SCOPE_TOKEN")
        .env_remove("SHOAL_COMPLETE")
        .env_remove("NO_COLOR")
        .env("TERM", "xterm-256color")
        .envs(env.iter().copied())
        .stdin(Stdio::null());
    let mut master = terminal.map(|stdout| {
        let (mut master, mut slave) = (-1, -1);
        // SAFETY: openpty initializes both descriptors; File owns them below.
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        let slave = unsafe { File::from_raw_fd(slave) };
        if stdout {
            command.stdout(slave);
        } else {
            command.stderr(slave);
        }
        let master = unsafe { File::from_raw_fd(master) };
        assert_ne!(
            unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) },
            -1
        );
        master
    });
    let output = command.output().unwrap();
    // Keep the slave open while draining: macOS can discard unread data on close.
    let mut transcript = String::new();
    if let Some(master) = &mut master {
        match master.read_to_string(&mut transcript) {
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("read terminal: {error}"),
        }
    }
    (output, transcript)
}

#[test]
fn terminal_styles_respect_redirection_json_and_environment() {
    for (args, stdout, marker) in [
        (
            vec!["daemon", "status"],
            true,
            "\x1b[33mDaemon is not running\x1b[0m",
        ),
        (vec!["cd", "-"], false, "\x1b[1;31merror:\x1b[0m"),
    ] {
        let env = [("SHOAL_PREVIOUS_DIR", "/nonexistent-shoal-color-test")];
        let (output, text) = run(&args, Some(stdout), &env);
        assert!(!output.status.success());
        assert!(
            text.contains(marker),
            "args={args:?} text={text:?} output={output:?}"
        );
        let (piped, _) = run(&args, Some(!stdout), &env);
        let bytes = if stdout { piped.stdout } else { piped.stderr };
        assert!(!bytes.contains(&0x1b));
        assert!(!bytes.is_empty());
        for opt_out in [("NO_COLOR", "1"), ("TERM", "dumb")] {
            let (_, text) = run(&args, Some(stdout), &[env[0], opt_out]);
            assert!(!text.contains('\x1b'), "{text:?}");
        }
        let mut json_args = vec!["--json"];
        json_args.extend(args);
        let (_, text) = run(&json_args, Some(stdout), &env);
        assert!(!text.contains('\x1b'));
        serde_json::from_str::<serde_json::Value>(&text).unwrap();
    }
    let (_, script) = run(&["shell", "init"], Some(true), &[]);
    assert!(!script.contains('\x1b'));
    assert!(script.contains("shoal"));
}
