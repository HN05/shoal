use crate::support::{Fixture, wait_until};
use serde_json::Value;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
};

/// A stand-in for Happy's CLI that records how it was started, writes a line
/// of output, and then waits to be stopped.
fn install_fake_happy(fixture: &Fixture) -> PathBuf {
    let bin = fixture.root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let script = bin.join("happy");
    fs::write(
        &script,
        r#"#!/bin/sh
{
  printf 'cwd=%s\n' "$PWD"
  printf 'args='; printf '%s\0' "$@"; printf '\n'
  printf 'scope=%s\nexecution=%s\nworkspace=%s\nport=%s\n' "$SHOAL_SCOPE_TOKEN" "$SHOAL_EXECUTION_ID" "$SHOAL_WORKSPACE" "$SHOAL_PORT_WEB"
  printf 'reconnect=%s|%s|%s|%s|%s|%s\n' "$HAPPY_RECONNECT_SESSION_ID" "$HAPPY_RECONNECT_ENCRYPTION_KEY" "$HAPPY_RECONNECT_ENCRYPTION_VARIANT" "$HAPPY_RECONNECT_SEQ" "$HAPPY_RECONNECT_METADATA_VERSION" "$HAPPY_RECONNECT_AGENT_STATE_VERSION"
  if read -r _line; then printf 'stdin=data\n'; else printf 'stdin=eof\n'; fi
  test -t 1 && printf 'stdout=tty\n' || printf 'stdout=notty\n'
} > "$HAPPY_RECORD"
echo "hello from happy"
echo "happy stderr" >&2
# A real session connects to Happy's server and heartbeats; tell the fake server.
if [ -n "$HAPPY_RECONNECT_SESSION_ID" ] && [ -n "$HAPPY_SERVER_URL" ]; then
  sleep 1
  curl -s -X POST "$HAPPY_SERVER_URL/test/activate/$HAPPY_RECONNECT_SESSION_ID" > /dev/null
fi
sleep 300
"#,
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    fixture.root.path().join("happy-record")
}

fn process_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[test]
fn happy_sessions_launch_detached_tracked_and_stop_with_the_workspace() {
    let fixture = Fixture::new();
    let record = install_fake_happy(&fixture);
    let state_file = fixture.root.path().join(".happy/daemon.state.json");
    for (operation, agent) in [("stop", "codex"), ("rm", "claude")] {
        let name = format!("happy-{agent}");
        let launch = if agent == "codex" {
            // No Happy daemon state: warn, but launch anyway.
            let output = fixture
                .command()
                .args([
                    "--json",
                    "add",
                    fixture.repo.to_str().unwrap(),
                    &name,
                    "--agent",
                    "happy-codex",
                    "--",
                    "--yolo",
                ])
                .env("HAPPY_RECORD", &record)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("daemon.state.json") && stderr.contains("happy daemon start"),
                "{stderr}"
            );
            let mut lines = output
                .stdout
                .split(|b| *b == b'\n')
                .filter(|l| !l.is_empty());
            let workspace: Value = serde_json::from_slice(lines.next().unwrap()).unwrap();
            assert_eq!(workspace["name"], name);
            let launch: Value = serde_json::from_slice(lines.next().unwrap()).unwrap();
            assert!(lines.next().is_none());
            assert_eq!(launch["happy_daemon_recorded"], false);
            assert_eq!(launch["prompt_file"], Value::Null);
            let config: toml::Value = toml::from_str(
                &fs::read_to_string(fixture.root.path().join(".codex/config.toml")).unwrap(),
            )
            .unwrap();
            let key = fs::canonicalize(workspace["path"].as_str().unwrap()).unwrap();
            assert_eq!(
                config["projects"][key.to_str().unwrap()]["trust_level"].as_str(),
                Some("trusted")
            );
            launch
        } else {
            let workspace = fixture.add(&name);
            fixture.ok(&["port", "acquire", "web", &name, "--reason", "server"]);
            fs::create_dir_all(state_file.parent().unwrap()).unwrap();
            fs::write(&state_file, "{}").unwrap();
            let claude_config = fixture.root.path().join(".claude.json");
            fs::write(&claude_config, "{}").unwrap();
            let output = fixture
                .command()
                .args(["--json", "happy", "claude", &name, "--", "--model", "test"])
                .env("HAPPY_RECORD", &record)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            assert_eq!(output.stderr, b"", "{output:?}");
            let launch: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(launch["workspace"]["id"], workspace["id"]);
            assert_eq!(launch["happy_daemon_recorded"], true);
            // Detached Claude launches skip the trust dialog like `shoal claude`.
            let config: Value =
                serde_json::from_str(&fs::read_to_string(&claude_config).unwrap()).unwrap();
            let key = fs::canonicalize(workspace["path"].as_str().unwrap()).unwrap();
            assert_eq!(
                config["projects"][key.to_str().unwrap()]["hasTrustDialogAccepted"],
                true
            );
            launch
        };
        assert_eq!(launch["agent"], agent);
        let pid = launch["pid"].as_u64().unwrap() as u32;
        let execution_id = launch["execution_id"].as_str().unwrap();
        let log = PathBuf::from(launch["log"].as_str().unwrap());
        assert!(log.starts_with(fixture.root.path().join("state/workspaces")));
        let inspection = fixture.ok(&["inspect", &name]);
        let executions = inspection["executions"].as_array().unwrap();
        assert_eq!(executions.len(), 1, "{inspection}");
        assert_eq!(executions[0]["id"], execution_id);
        assert_eq!(executions[0]["group_id"], pid);
        assert_eq!(executions[0]["state"], "running");
        wait_until("fake happy record", || record.exists());
        let recorded = fs::read_to_string(&record).unwrap();
        let path = fs::canonicalize(inspection["workspace"]["path"].as_str().unwrap()).unwrap();
        assert!(
            recorded.contains(&format!("cwd={}\n", path.display())),
            "{recorded}"
        );
        let expected_args = if agent == "codex" {
            "args=codex\0--happy-starting-mode\0remote\0--started-by\0daemon\0--yolo\0\n"
        } else {
            "args=claude\0--happy-starting-mode\0remote\0--started-by\0daemon\0--model\0test\0\n"
        };
        assert!(recorded.contains(expected_args), "{recorded}");
        assert!(recorded.contains(&format!("execution={execution_id}\n")));
        assert!(recorded.contains(&format!("workspace={name}\n")));
        assert!(!recorded.contains("scope=\n"), "{recorded}");
        assert!(recorded.contains("stdin=eof\nstdout=notty\n"), "{recorded}");
        if agent == "claude" {
            let port = fixture.ok(&["port", &name])["reserved"][0]["port"]
                .as_u64()
                .unwrap();
            assert!(recorded.contains(&format!("port={port}\n")), "{recorded}");
        }
        wait_until("happy output in the log", || {
            fs::read_to_string(&log).is_ok_and(|text| text.contains("hello from happy"))
        });
        let text = fs::read_to_string(&log).unwrap();
        assert!(text.starts_with("shoal: starting happy "), "{text}");
        assert!(text.contains("happy stderr"), "{text}");
        assert!(process_alive(pid));

        fixture.ok(&[operation, &name]);
        wait_until("happy to exit", || !process_alive(pid));
        if operation == "stop" {
            assert_eq!(
                fixture.ok(&["inspect", &name])["executions"],
                serde_json::json!([])
            );
            assert!(log.exists());
            fixture.ok(&["rm", &name]);
        }
        assert!(
            !log.parent().unwrap().exists(),
            "workspace run data removed"
        );
        fs::remove_file(&record).unwrap();
    }
}

#[test]
fn happy_issue_prompts_reach_claude_and_are_saved_for_codex() {
    let fixture = Fixture::new();
    let config_dir = fixture.root.path().join(".config/shoal");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(config_dir.join("agent-template.md"), "General {workspace}").unwrap();
    fs::write(
        config_dir.join("issue-template.md"),
        "Issue {title}: {body}",
    )
    .unwrap();
    let record = install_fake_happy(&fixture);
    let gh = fixture.root.path().join("bin/gh");
    let title = "Fix API timeout";
    let body = "Keep the connection alive.";
    fs::write(
        &gh,
        format!(
            "#!/bin/sh\nprintf '%s' '{}'\n",
            serde_json::json!({"number": 34, "title": title, "body": body})
        ),
    )
    .unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o700)).unwrap();
    fixture.add_github_origin();
    for agent in ["happy-claude", "happy-codex"] {
        let output = fixture
            .command()
            .args([
                "--json",
                "add",
                fixture.repo.to_str().unwrap(),
                "--base",
                "HEAD",
                "--issue",
                "34",
                "--agent",
                agent,
                "--",
                "--model",
                "test",
            ])
            .env("HAPPY_RECORD", &record)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let launch: Value =
            serde_json::from_slice(output.stdout.split(|b| *b == b'\n').nth(1).unwrap()).unwrap();
        let name = launch["workspace"]["name"].as_str().unwrap().to_owned();
        assert_eq!(name, "issue-34-fix-api-timeout");
        wait_until("fake happy record", || record.exists());
        let recorded = fs::read_to_string(&record).unwrap();
        // The prompt spans lines; the record ends the NUL-separated list before `scope=`.
        let args: Vec<&str> = recorded
            .split_once("args=")
            .unwrap()
            .1
            .split_once("\nscope=")
            .unwrap()
            .0
            .split('\0')
            .collect();
        let stderr = String::from_utf8_lossy(&output.stderr);
        if agent == "happy-claude" {
            assert_eq!(
                args[..5],
                [
                    "claude",
                    "--happy-starting-mode",
                    "remote",
                    "--started-by",
                    "daemon"
                ]
            );
            assert!(
                args[5].contains(title) && args[5].contains(body),
                "{args:?}"
            );
            assert_eq!(
                args[6..8],
                ["--append-system-prompt", "General issue-34-fix-api-timeout"]
            );
            assert_eq!(args[8..10], ["--model", "test"]);
            assert_eq!(launch["prompt_file"], Value::Null);
            assert_eq!(launch["prompt_delivered"], true, "{launch}");
            assert!(!stderr.contains("initial prompt"), "{stderr}");
        } else {
            assert_eq!(
                args[..7],
                [
                    "codex",
                    "--happy-starting-mode",
                    "remote",
                    "--started-by",
                    "daemon",
                    "--model",
                    "test"
                ]
            );
            // Not logged in to Happy: the prompt is saved and the user is told.
            let prompt_file = PathBuf::from(launch["prompt_file"].as_str().unwrap());
            let prompt = fs::read_to_string(&prompt_file).unwrap();
            assert_eq!(
                prompt,
                format!("General issue-34-fix-api-timeout\n\nIssue {title}: {body}")
            );
            assert_eq!(launch["prompt_delivered"], false);
            assert_eq!(launch["happy_session_id"], Value::Null);
            assert!(
                stderr.contains("cannot deliver the prompt through Happy")
                    && stderr.contains("access.key")
                    && stderr.contains(prompt_file.to_str().unwrap()),
                "{stderr}"
            );
            assert!(recorded.contains("reconnect=|||||\n"), "{recorded}");
        }
        let pid = launch["pid"].as_u64().unwrap() as u32;
        fixture.ok(&["rm", &name, "--yes", "--delete-branch"]);
        wait_until("happy to exit", || !process_alive(pid));
        fs::remove_file(&record).unwrap();
    }
}

/// A local stand-in for Happy's server; killed when dropped.
struct FakeHappyServer {
    child: Child,
    url: String,
    record: PathBuf,
}

impl FakeHappyServer {
    fn start(fixture: &Fixture) -> Self {
        use std::io::BufRead;
        let script = fixture.root.path().join("happy_server.py");
        fs::write(&script, include_str!("../fixtures/happy_server.py")).unwrap();
        let record = fixture.root.path().join("happy-server.json");
        let mut child = Command::new("python3")
            .arg(&script)
            .arg(&record)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut port = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut port)
            .unwrap();
        Self {
            child,
            url: format!("http://127.0.0.1:{}", port.trim()),
            record,
        }
    }

    fn state(&self) -> Value {
        serde_json::from_str(&fs::read_to_string(&self.record).unwrap()).unwrap()
    }
}

impl Drop for FakeHappyServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn happy_codex_prompts_are_delivered_through_a_seeded_session() {
    use base64::Engine;
    let base64 = base64::engine::general_purpose::STANDARD;
    let fixture = Fixture::new();
    let record = install_fake_happy(&fixture);
    let server = FakeHappyServer::start(&fixture);
    let happy_home = fixture.root.path().join(".happy");
    fs::create_dir_all(&happy_home).unwrap();
    fs::write(
        happy_home.join("settings.json"),
        r#"{"machineId": "machine-1"}"#,
    )
    .unwrap();
    fs::write(happy_home.join("daemon.state.json"), "{}").unwrap();
    let key = base64.encode([7u8; 32]);
    let prompt = "Fix the login bug; keep the API stable.";
    let plaintext = serde_json::json!({
        "role": "user",
        "content": {"type": "text", "text": prompt},
        "meta": {"sentFrom": "shoal"},
    })
    .to_string()
    .len();
    for (variant, credentials) in [
        (
            "dataKey",
            serde_json::json!({"token": "test-token", "encryption": {"publicKey": key, "machineKey": key}}),
        ),
        (
            "legacy",
            serde_json::json!({"token": "test-token", "secret": key}),
        ),
    ] {
        fs::write(happy_home.join("access.key"), credentials.to_string()).unwrap();
        let name = format!("seeded-{}", variant.to_lowercase());
        fixture.add(&name);
        let output = fixture
            .command()
            .args([
                "--json", "happy", "codex", &name, "--prompt", prompt, "--", "--yolo",
            ])
            .env("HAPPY_RECORD", &record)
            .env("HAPPY_SERVER_URL", &server.url)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let launch: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(launch["prompt_delivered"], true, "{launch}");
        let session_id = launch["happy_session_id"].as_str().unwrap().to_owned();
        assert!(session_id.starts_with("session-"));
        assert_eq!(
            fs::read_to_string(launch["prompt_file"].as_str().unwrap()).unwrap(),
            prompt
        );
        assert!(
            !String::from_utf8_lossy(&output.stderr).contains("warning"),
            "{output:?}"
        );

        // The CLI was launched attached to the seeded session.
        wait_until("fake happy record", || record.exists());
        let recorded = fs::read_to_string(&record).unwrap();
        let reconnect = recorded
            .lines()
            .find_map(|line| line.strip_prefix("reconnect="))
            .unwrap();
        let fields: Vec<&str> = reconnect.split('|').collect();
        assert_eq!(fields[0], session_id);
        assert_eq!(base64.decode(fields[1]).unwrap().len(), 32);
        assert_eq!(fields[2..], [variant, "0", "1", "1"]);
        if variant == "legacy" {
            assert_eq!(fields[1], key, "legacy sessions use the account secret");
        }
        assert!(recorded.contains(
            "args=codex\0--happy-starting-mode\0remote\0--started-by\0daemon\0--yolo\0\n"
        ));

        // The server saw a session created the way happy-cli creates them,
        // a wait for it to come alive, and one encrypted user message.
        let state = server.state();
        let requests = state["requests"].as_array().unwrap();
        let created = requests
            .iter()
            .rev()
            .find(|r| r["path"] == "/v1/sessions" && r["body"]["tag"].is_string())
            .unwrap();
        assert_eq!(created["authorization"], "Bearer test-token");
        assert!(created["client"].as_str().unwrap().starts_with("shoal/"));
        let metadata = base64
            .decode(created["body"]["metadata"].as_str().unwrap())
            .unwrap();
        let sealed = &created["body"]["dataEncryptionKey"];
        if variant == "dataKey" {
            assert_eq!(metadata[0], 0);
            assert_eq!(
                base64.decode(sealed.as_str().unwrap()).unwrap().len(),
                1 + 32 + 24 + 32 + 16
            );
        } else {
            assert!(metadata.len() > 24 + 16);
            assert_eq!(*sealed, Value::Null);
        }
        assert!(requests.iter().any(|r| r["path"] == "/v2/sessions/active"));
        let messages = state["messages"][&session_id].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert!(!messages[0]["localId"].as_str().unwrap().is_empty());
        let content = base64
            .decode(messages[0]["content"].as_str().unwrap())
            .unwrap();
        let expected = if variant == "dataKey" {
            1 + 12 + plaintext + 16
        } else {
            24 + plaintext + 16
        };
        assert_eq!(content.len(), expected);
        assert_eq!(
            requests
                .iter()
                .filter(|r| r["path"] == "/v1/sessions" && r["method"] == "POST")
                .count(),
            if variant == "dataKey" { 1 } else { 2 }
        );

        let pid = launch["pid"].as_u64().unwrap() as u32;
        fixture.ok(&["rm", &name]);
        wait_until("happy to exit", || !process_alive(pid));
        fs::remove_file(&record).unwrap();
    }

    // Without a prompt nothing is seeded; a wrong token fails delivery softly.
    fs::write(
        happy_home.join("access.key"),
        r#"{"token": "wrong", "secret": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="}"#,
    )
    .unwrap();
    fixture.add("unseeded");
    let output = fixture
        .command()
        .args(["--json", "happy", "codex", "unseeded", "--prompt", "hello"])
        .env("HAPPY_RECORD", &record)
        .env("HAPPY_SERVER_URL", &server.url)
        // Launched from inside another seeded session: its attachment must not leak.
        .env("HAPPY_RECONNECT_SESSION_ID", "parent-session")
        .env("HAPPY_RECONNECT_ENCRYPTION_KEY", &key)
        .env("HAPPY_RECONNECT_ENCRYPTION_VARIANT", "legacy")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let launch: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(launch["prompt_delivered"], false);
    assert_eq!(launch["happy_session_id"], Value::Null);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot deliver the prompt through Happy") && stderr.contains("401"),
        "{stderr}"
    );
    wait_until("fake happy record", || record.exists());
    let recorded = fs::read_to_string(&record).unwrap();
    assert!(recorded.contains("reconnect=|||||\n"), "{recorded}");
    let pid = launch["pid"].as_u64().unwrap() as u32;
    fixture.ok(&["rm", "unseeded"]);
    wait_until("happy to exit", || !process_alive(pid));

    // A seeded session that no launch will ever attach to is deleted again.
    fs::write(
        happy_home.join("access.key"),
        serde_json::json!({"token": "test-token", "secret": key}).to_string(),
    )
    .unwrap();
    fs::remove_file(fixture.root.path().join("bin/happy")).unwrap();
    fixture.add("orphan");
    let output = fixture
        .command()
        .args(["--json", "happy", "codex", "orphan", "--prompt", "hello"])
        .env("HAPPY_SERVER_URL", &server.url)
        // Only the fixture directory and system tools: a real happy must not be found.
        .env(
            "PATH",
            format!(
                "{}:/usr/bin:/bin",
                fixture.root.path().join("bin").display()
            ),
        )
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    let state = server.state();
    let methods: Vec<&str> = state["requests"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["path"].as_str().unwrap().starts_with("/v1/sessions"))
        .map(|r| r["method"].as_str().unwrap())
        .collect();
    assert_eq!(methods.last(), Some(&"DELETE"), "{methods:?}");
    assert_eq!(state["sessions"].as_object().unwrap().len(), 2, "{state}");
}
