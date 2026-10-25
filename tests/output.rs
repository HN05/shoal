#[path = "support/pty.rs"]
mod pty;

mod support;

use std::{
    io::Read,
    process::{Output, Stdio},
    sync::mpsc,
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
    reply: Option<(bool, serde_json::Value)>,
) -> (Output, String) {
    run_with_replies(args, terminal, env, reply.into_iter().collect())
}

fn run_with_replies(
    args: &[&str],
    terminal: Option<bool>,
    env: &[(&str, &str)],
    replies: Vec<(bool, serde_json::Value)>,
) -> (Output, String) {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let (progress, drawn) = mpsc::channel();
    let server = (!replies.is_empty()).then(|| {
        let listener = support::fake_daemon::bind_fake_daemon(root.path());
        thread::spawn(move || {
            for (wait_for_progress, reply) in replies {
                let (stream, _) = listener.accept().unwrap();
                support::fake_daemon::answer(stream, |_| {
                    if wait_for_progress {
                        drawn
                            .recv_timeout(Duration::from_secs(60))
                            .expect("progress was not drawn");
                    }
                    reply
                });
            }
        })
    });
    let mut command = support::cli(root.path());
    command
        .args(args)
        .env_remove("NO_COLOR")
        .env("TERM", "xterm-256color")
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut master = terminal.map(|stdout| {
        let (master, slave) = pty::open();
        if stdout {
            command.stdout(slave);
        } else {
            command.stderr(slave);
        }
        master
    });
    let mut child = command.spawn().unwrap();
    let mut transcript = String::new();
    if support::wait::try_wait_for(|| {
        if let Some(master) = &mut master {
            drain(master, &mut transcript);
            if transcript.contains("s)") {
                let _ = progress.send(());
            }
        }
        child.try_wait().unwrap()
    })
    .is_none()
    {
        let _ = child.kill();
        let _ = child.wait();
        panic!("output command stalled: {transcript:?}");
    }
    let output = child.wait_with_output().unwrap();
    if let Some(server) = server {
        server.join().unwrap();
    }
    // Keep the slave open while draining: macOS can discard unread data on close.
    if let Some(master) = &mut master {
        drain(master, &mut transcript);
    }
    (output, transcript)
}

fn drain(master: &mut std::fs::File, transcript: &mut String) {
    match master.read_to_string(transcript) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
        Err(error) => panic!("read terminal: {error}"),
    }
}

#[test]
fn root_help_entry_points_are_grouped_and_subcommand_help_stays_specific() {
    let root = tempfile::tempdir().unwrap();
    let mut outputs = Vec::new();
    for args in [vec![], vec!["-h"], vec!["--help"], vec!["help"]] {
        let output = support::cli(root.path())
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(help.contains("Workspaces:\n"), "{help}");
        assert!(help.contains("shoal exec fix-login -- cargo test"));
        assert!(!help.contains('\x1b'));
        outputs.push(help);
    }
    assert!(outputs.windows(2).all(|pair| pair[0] == pair[1]));
    for args in [["cd", "--help"], ["help", "cd"]] {
        let output = support::cli(root.path()).args(args).output().unwrap();
        assert!(output.status.success(), "{output:?}");
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(help.contains("Usage: shoal cd"), "{help}");
        assert!(help.contains("Omit the workspace to open the picker"));
        assert!(!help.contains("Workspaces:\n"));
    }
    assert!(!root.path().join("state").exists());
}

// A reader that has already gone, like `head` after its lines, ends text,
// JSON and help output with the status a shell reports for SIGPIPE.
#[test]
fn a_closed_stdout_ends_commands_quietly() {
    let root = tempfile::tempdir().unwrap();
    for args in [
        vec!["skill"],
        vec!["--json", "skill", "worker"],
        vec![],
        vec!["repo"],
        vec!["--help"],
        vec!["--version"],
        vec!["custom-tool", "--help"],
    ] {
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let output = support::cli(root.path())
            .args(&args)
            .stdin(Stdio::null())
            .stdout(writer)
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(141), "{args:?}: {output:?}");
        assert!(output.stderr.is_empty(), "{args:?}: {output:?}");
    }
}

// Output without a final newline stays buffered until the command exits.
#[test]
fn a_closed_stdout_ends_unterminated_output_quietly() {
    let root = tempfile::tempdir().unwrap();
    let listener = support::fake_daemon::bind_fake_daemon(root.path());
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        support::fake_daemon::answer(stream, |_| {
            serde_json::json!({
                "type": "repository_config",
                "data": {"repository_id": "test", "toml": "focus=true"}
            })
        });
    });
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let output = support::cli(root.path())
        .args(["repo", "config", "/test"])
        .stdin(Stdio::null())
        .stdout(writer)
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    server.join().unwrap();
    assert_eq!(output.status.code(), Some(141), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
}

#[test]
fn repository_progress_respects_output_mode_and_clears_before_errors() {
    let success = serde_json::json!({
        "type": "repository",
        "data": {"id": "test", "path": "/test", "source": "/test", "last_used": 0}
    });
    let repositories = serde_json::json!({
        "type": "repositories",
        "data": [success["data"].clone()]
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
        let (output, text) =
            run_with_reply(&args, terminal, &env, Some((visible, success.clone())));
        assert!(output.status.success(), "{output:?} {text:?}");
        assert_eq!(text.contains("Registering repository"), visible, "{text:?}");
        assert!(!output.stdout.contains(&b'\r'));
        assert!(output.stderr.is_empty());
        if visible {
            assert!(text.ends_with(" \r"), "{text:?}");
        }
        if json {
            assert!(text.is_empty());
            let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(value["id"], "test");
        }
    }
    let (output, text) = run_with_replies(
        &["repo", "rm", "/test", "--yes"],
        Some(false),
        &[("NO_COLOR", "1")],
        vec![
            (false, repositories),
            (
                true,
                serde_json::json!({
                    "type": "error", "data": {"code": "test", "message": "removal failed"}
                }),
            ),
        ],
    );
    assert!(!output.status.success());
    assert!(text.contains("Removing repository"), "{text:?}");
    assert!(text.contains(" \rerror: test: removal failed"), "{text:?}");
}

#[test]
fn install_progress_clears_on_failure_and_leaves_json_and_preview_clean() {
    for json in [false, true] {
        let args = if json {
            vec!["--json", "daemon", "install"]
        } else {
            vec!["daemon", "install"]
        };
        let (output, text) = run_with_reply(
            &args,
            Some(false),
            &[("NO_COLOR", "1")],
            Some((
                !json,
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
    let (output, text) = run(&["daemon", "install", "--dry-run"], Some(false), &[]);
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
        let (output, text) = run(&args, Some(stdout), &[]);
        assert!(!output.status.success());
        assert!(
            text.contains(marker),
            "args={args:?} text={text:?} output={output:?}"
        );
        let (piped, _) = run(&args, Some(!stdout), &[]);
        let bytes = if stdout { piped.stdout } else { piped.stderr };
        assert!(!bytes.contains(&0x1b));
        assert!(!bytes.is_empty());
        for opt_out in [("NO_COLOR", "1"), ("TERM", "dumb")] {
            let (_, text) = run(&args, Some(stdout), &[opt_out]);
            assert!(!text.contains('\x1b'), "{text:?}");
        }
        let mut json_args = vec!["--json"];
        json_args.extend(args);
        let (_, text) = run(&json_args, Some(stdout), &[]);
        assert!(!text.contains('\x1b'));
        serde_json::from_str::<serde_json::Value>(&text).unwrap();
    }
    let (_, script) = run(&["shell", "init"], Some(true), &[]);
    assert!(!script.contains('\x1b'));
    assert!(script.contains("shoal"));
}
