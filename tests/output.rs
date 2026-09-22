use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Write},
    os::fd::{AsRawFd, FromRawFd},
    os::unix::net::UnixListener,
    process::{Command, Output, Stdio},
    thread,
    time::Duration,
};

// Run short, offline commands with either output stream attached to a terminal.
fn run(args: &[&str], terminal: Option<bool>, env: &[(&str, &str)]) -> (Output, String) {
    run_with_reply(args, terminal, env, None)
}

fn run_with_reply(
    args: &[&str],
    terminal: Option<bool>,
    env: &[(&str, &str)],
    reply: Option<(Duration, serde_json::Value)>,
) -> (Output, String) {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let server = reply.map(|(delay, mut reply)| {
        let listener = UnixListener::bind(root.path().join("daemon.sock")).unwrap();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            reply["protocol"] = request["protocol"].clone();
            reply["id"] = request["id"].clone();
            thread::sleep(delay);
            writeln!(stream, "{reply}").unwrap();
        })
    });
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
    if let Some(server) = server {
        server.join().unwrap();
    }
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
fn repository_progress_respects_output_mode_and_clears_before_errors() {
    let success = serde_json::json!({
        "type": "repository",
        "data": {"id": "test", "path": "/test", "source": "/test", "last_used": 0}
    });
    for (terminal, json, env, visible) in [
        (Some(false), false, vec![], true),
        (Some(false), false, vec![("NO_COLOR", "1")], true),
        (Some(false), true, vec![], false),
        (Some(false), false, vec![("TERM", "dumb")], false),
        (Some(true), false, vec![], false),
        (None, false, vec![], false),
    ] {
        let mut args = vec!["repo", "add", "/test"];
        if json {
            args.insert(0, "--json");
        }
        let (output, text) = run_with_reply(
            &args,
            terminal,
            &env,
            Some((Duration::from_millis(1200), success.clone())),
        );
        assert!(output.status.success(), "{output:?} {text:?}");
        assert_eq!(text.contains("Registering repository"), visible, "{text:?}");
        assert!(!output.stdout.contains(&b'\r'));
        assert!(output.stderr.is_empty());
        if visible {
            assert!(text.contains("(1s)"), "{text:?}");
            assert!(text.contains("| Registering") && text.contains("/ Registering"));
            assert!(text.ends_with(" \r"), "{text:?}");
        }
        if json {
            assert!(text.is_empty());
            let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(value["id"], "test");
        }
    }
    let (output, text) = run_with_reply(
        &["repo", "add", "/test"],
        Some(false),
        &[],
        Some((Duration::ZERO, success)),
    );
    assert!(output.status.success());
    assert!(text.is_empty(), "{text:?}");

    let (output, text) = run_with_reply(
        &["repo", "rm", "/test", "--yes"],
        Some(false),
        &[("NO_COLOR", "1")],
        Some((
            Duration::from_millis(700),
            serde_json::json!({
                "type": "error", "data": {"code": "test", "message": "removal failed"}
            }),
        )),
    );
    assert!(!output.status.success());
    assert!(text.contains("Removing repository"), "{text:?}");
    assert!(text.contains(" \rerror: test: removal failed"), "{text:?}");
}

#[test]
fn install_progress_clears_on_failure_and_leaves_json_and_preview_clean() {
    for json in [false, true] {
        let args = if json {
            vec!["--json", "install"]
        } else {
            vec!["install"]
        };
        let (output, text) = run_with_reply(
            &args,
            Some(false),
            &[("NO_COLOR", "1")],
            Some((
                Duration::from_millis(700),
                serde_json::json!({
                    "type": "error", "data": {"code": "test", "message": "status failed"}
                }),
            )),
        );
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        if json {
            let value: serde_json::Value = serde_json::from_str(&text).unwrap();
            assert_eq!(value["error"]["message"], "test: status failed");
        } else {
            assert!(text.contains("Checking daemon"), "{text:?}");
            assert!(text.contains(" \rerror: test: status failed"), "{text:?}");
        }
    }
    let (output, text) = run(&["install", "--dry-run"], Some(false), &[]);
    assert!(output.status.success());
    assert!(text.is_empty());
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
