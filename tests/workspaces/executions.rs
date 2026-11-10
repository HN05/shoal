//! The execution wrapper: terminal settings, pipes, exit codes, environment
//! and connected executions.
use super::*;

#[test]
fn execution_restores_terminal_settings_after_exit_and_stop() {
    use std::io::Read;

    let fixture = Fixture::new();
    fixture.add("terminal");
    let agent = fixture.root.path().join("raw-agent.py");
    install_test_script(
        &agent,
        r#"#!/usr/bin/env python3
import os, pathlib, signal, sys, time, tty
mode = sys.argv[1] if len(sys.argv) > 1 else 'success'
tty.setraw(0)
# Emulator modes are independent of termios. Write to the terminal even when
# the command's output streams are pipes (as in the driver below).
with open('/dev/tty', 'wb', buffering=0) as terminal:
    for mode_number in (1000, 1002, 1003, 1005, 1006, 1015, 1016, 1004, 1007, 2004, 1):
        terminal.write(f'\x1b[?{mode_number}h'.encode())
    terminal.write(b'\x1b=\x1b[>4;2m\x1b[=15u')
pathlib.Path('raw-ready').touch()
if mode in ('success', 'exit'):
    sys.exit(0 if mode == 'success' else 7)
if mode == 'killed':
    os.kill(os.getpid(), signal.SIGKILL)
while True:
    time.sleep(0.01)
"#,
    );
    for (mode, term) in [
        ("success", Some("xterm-256color")),
        ("exit", Some("xterm-256color")),
        ("killed", Some("xterm-256color")),
        ("interrupt", Some("xterm-256color")),
        ("stop", Some("xterm-256color")),
        ("success", Some("dumb")),
        ("success", Some("")),
        ("success", None),
    ] {
        let (mut master, slave) = pty::open();
        let mut driver = support::isolated(fixture.root.path(), "python3");
        match term {
            Some(term) => driver.env("TERM", term),
            None => driver.env_remove("TERM"),
        };
        let mut child = driver
            .args([
                "-c",
                r#"import fcntl, os, pathlib, signal, subprocess, sys, termios, time
binary, agent, mode = sys.argv[1:]
os.setsid()
fcntl.ioctl(0, termios.TIOCSCTTY, 0)
# Preserve deliberate caller customization too, rather than resetting to sane.
settings = termios.tcgetattr(0)
settings[6][termios.VEOF] = b'\x1d'
termios.tcsetattr(0, termios.TCSANOW, settings)
def terminal_settings():
    settings = termios.tcgetattr(0)
    # PENDIN describes pending input reprocessing, which the kernel can set
    # when canonical mode is restored; it is not a caller-controlled setting.
    settings[3] &= ~getattr(termios, 'PENDIN', 0)
    return settings
before = terminal_settings()
group = os.tcgetpgrp(0)
command = [binary, 'run', '--workspace', 'terminal', '--', 'python3', agent, mode]
child = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
try:
    if mode in ('interrupt', 'stop'):
        import json
        workspace = pathlib.Path(json.loads(subprocess.check_output(
            [binary, '--json', 'status', 'terminal'], text=True))['workspace']['path'])
        deadline = time.monotonic() + 10
        while not (workspace / 'raw-ready').exists():
            assert child.poll() is None, child.communicate()
            assert time.monotonic() < deadline, 'agent did not enter raw mode'
            time.sleep(0.01)
        if mode == 'interrupt':
            child.send_signal(signal.SIGTERM)
        else:
            subprocess.run([binary, 'stop', 'terminal'], check=True, capture_output=True, timeout=15)
    stdout, stderr = child.communicate(timeout=15)
    expected = {'success': 0, 'exit': 7, 'killed': 137, 'interrupt': 143, 'stop': 143}[mode]
    assert child.returncode == expected, (child.returncode, stdout, stderr)
    assert b'\x1b' not in stdout + stderr, (stdout, stderr)
    assert os.tcgetpgrp(0) == group, 'foreground group was not restored'
    assert terminal_settings() == before, (mode, before, terminal_settings())
finally:
    if child.poll() is None:
        child.kill()
        child.wait()
"#,
                support::shoal_binary(),
                agent.to_str().unwrap(),
                mode,
            ])
            .stdin(slave.try_clone().unwrap())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut transcript = String::new();
        if try_wait_for(|| {
            // Drain while the driver runs: closing a PTY can wait for unread
            // output on macOS, including when the driver has been killed.
            let _ = master.read_to_string(&mut transcript);
            child.try_wait().unwrap()
        })
        .is_none()
        {
            let _ = child.kill();
            let _ = master.read_to_string(&mut transcript);
            let _ = child.wait();
            panic!("terminal driver hung: {mode} {term:?}: {transcript:?}");
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{mode} {term:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        // The nonblocking master may report WouldBlock or EIO once drained.
        let _ = master.read_to_string(&mut transcript);
        let resets_input = term == Some("xterm-256color");
        for number in [
            1000, 1002, 1003, 1005, 1006, 1015, 1016, 1004, 1007, 2004, 1,
        ] {
            let enabled = transcript.rfind(&format!("\x1b[?{number}h")).unwrap();
            let disabled = transcript.rfind(&format!("\x1b[?{number}l"));
            if resets_input {
                assert!(
                    disabled.unwrap() > enabled,
                    "{mode}: input mode {number} leaked"
                );
            } else {
                assert!(disabled.is_none(), "{term:?}: unnecessary terminal escape");
            }
        }
        for (enabled, disabled) in [
            ("\x1b=", "\x1b>"),
            ("\x1b[>4;2m", "\x1b[>4;0m"),
            ("\x1b[=15u", "\x1b[=0u"),
        ] {
            if resets_input {
                assert!(
                    transcript.rfind(disabled).unwrap() > transcript.rfind(enabled).unwrap(),
                    "{mode}: keyboard mode leaked"
                );
            } else {
                assert!(
                    !transcript.contains(disabled),
                    "{term:?}: unnecessary terminal escape"
                );
            }
        }
        let workspace = fixture.ok(&["status", "terminal"]);
        fs::remove_file(
            Path::new(workspace["workspace"]["path"].as_str().unwrap()).join("raw-ready"),
        )
        .unwrap();
    }
}

#[test]
fn execution_lets_its_command_stop_for_the_terminal_where_shoal_ignores_that() {
    use std::os::unix::process::CommandExt;

    let fixture = Fixture::new();
    fixture.add("stops");
    let mut command = fixture.command();
    // SAFETY: sigaction runs in the forked child before exec, without
    // allocating.
    unsafe {
        command.pre_exec(|| {
            let mut ignore: libc::sigaction = std::mem::zeroed();
            ignore.sa_sigaction = libc::SIG_IGN;
            for signal in [libc::SIGTTIN, libc::SIGTTOU] {
                libc::sigaction(signal, &ignore, std::ptr::null_mut());
            }
            Ok(())
        });
    }
    // Python starts its handlers from the signals the process inherits, and
    // unlike `ps -o sigignore`, this reads them on macOS too.
    let output = command
        .args(["run", "--workspace", "stops", "--", "python3", "-c"])
        .arg(
            "import signal\n\
             print(*(signal.getsignal(s) == signal.SIG_IGN\n\
             for s in (signal.SIGTTIN, signal.SIGTTOU)))",
        )
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "False False",
        "whether SIGTTIN and SIGTTOU are ignored: {output:?}"
    );
}

#[test]
fn execution_preserves_pipes_exit_code_environment_and_current_workspace() {
    let fixture = Fixture::new();
    let workspace = fixture.add("execute");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::create_dir(path.join("nested")).unwrap();
    let mut child = fixture
        .command()
        .current_dir(path.join("nested"))
        .args([
            "run",
            "--",
            "sh",
            "-c",
            "cat; printf '%s' \"$SHOAL_WORKSPACE\"; printf error >&2; exit 7",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"pipe:").unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(output.stdout, b"pipe:execute");
    assert_eq!(output.stderr, b"error");
    assert_eq!(
        fixture.ok(&["status", "execute"])["executions"],
        serde_json::json!([])
    );
    fixture.ok(&["rm", "execute"]);
}

#[test]
fn configured_environment_renders_for_tracked_commands() {
    let fixture = Fixture::with_config(Some(
        "[env]\nSESSION = '{workspace}'\nPROFILE = '{workspace_path}/.profile'\nREPO = '{repo}'\nBRANCH = '{branch}'\nLITERAL = '{workspace}'\nAPI_PORT = 'wrong'\nURL = 'http://localhost:{port.api}'\nGOCACHE = '{cache.go}'\n[caches.go]\nscope = 'shared'\n",
    ));
    fixture.add("env-values");
    let port = fixture.ok(&[
        "acquire",
        "port",
        "api",
        "--workspace",
        "env-values",
        "--env",
        "API_PORT",
    ]);
    let output = fixture
        .command()
        .args([
            "run",
            "--workspace", "env-values",
            "--",
            "sh",
            "-c",
            "printf '%s\\n%s\\n%s\\n%s\\n%s' \"$SESSION\" \"$PROFILE\" \"$REPO\" \"$BRANCH\" \"$LITERAL\"",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let lines: Vec<_> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(lines[0], "env-values");
    assert!(lines[1].ends_with("/env-values/.profile"));
    assert_eq!(lines[2], "repo with ' quotes & $literal");
    assert_eq!(lines[3], "env-values");
    assert_eq!(lines[4], "env-values");
    let output = fixture
        .command()
        .args([
            "run",
            "--workspace",
            "env-values",
            "--",
            "sh",
            "-c",
            "printf '%s|%s|%s' \"$API_PORT\" \"$URL\" \"$GOCACHE\"; touch \"$GOCACHE/entry\"",
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let output = String::from_utf8(output.stdout).unwrap();
    let (ports, cache) = output.rsplit_once('|').unwrap();
    assert_eq!(
        ports,
        format!("{}|http://localhost:{}", port["port"], port["port"])
    );
    assert!(cache.ends_with("/.caches/shared/go"), "{cache}");
    let exported = fixture.ok(&["internal", "env", "env-values"]);
    assert_eq!(exported["API_PORT"], port["port"].to_string());
    assert_eq!(exported["SESSION"], "env-values");
    assert_eq!(
        exported["URL"],
        format!("http://localhost:{}", port["port"])
    );
    assert_eq!(exported["GOCACHE"], cache);
    fixture.ok(&["rm", "env-values"]);
    // The repository keeps its caches until cleanup empties them.
    assert!(Path::new(cache).join("entry").exists());
    let report = fixture.ok(&["cleanup", "caches"]);
    let repository_id = &fixture.ok(&["repo", "ls"])[0]["id"];
    assert_eq!(
        report["caches"],
        serde_json::json!([{"repository": "repo with ' quotes & $literal", "repository_id": repository_id, "outcome": "emptied", "caches": ["go"]}])
    );
    assert!(!Path::new(cache).exists());
}

#[test]
fn stop_and_manual_removal_terminate_connected_executions() {
    let fixture = Fixture::new();
    for operation in ["stop", "rm"] {
        let workspace = fixture.add("running");
        let child = fixture
            .command()
            .args([
                "run",
                "--workspace",
                "running",
                "--",
                "sh",
                "-c",
                "while :; do sleep 1; done",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        wait_until("execution registration", || {
            !fixture.ok(&["status", "running"])["executions"]
                .as_array()
                .unwrap()
                .is_empty()
        });
        let output = fixture
            .command()
            .current_dir(workspace["path"].as_str().unwrap())
            .args(["--json", operation])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let output = child.wait_with_output().unwrap();
        assert!(!output.status.success());
        if operation == "stop" {
            fixture.ok(&["rm", "running"]);
        }
    }
}
