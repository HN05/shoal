//! Happy as an agent host: launches, prompts and chats.
use super::chats::{CHAT_NAME, after_temp_dirs};
use super::forge::github_issue;
use super::start::{GUIDANCE_START, after_guidance, strip_shoal_arguments};
use super::*;

/// A stand-in for Happy's CLI that records how it was started, writes a line
/// of output, and then waits to be stopped.
fn install_fake_happy(fixture: &Fixture) -> PathBuf {
    let bin = fixture.root.path().join("bin");
    let script = bin.join("happy");
    install_test_script_with_mode(&script, include_str!("../fixtures/happy.py"), 0o700);
    // Happy starts these itself; launches need them on PATH.
    for agent in ["claude", "codex"] {
        let path = bin.join(agent);
        if !path.exists() {
            install_test_script_with_mode(&path, "#!/bin/sh\n", 0o700);
        }
    }
    fixture.root.path().join("happy-record")
}

fn process_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

const HAPPY_ENABLED: &str = "[agents.happy]\nenabled = true\n";

#[test]
fn happy_agents_are_refused_until_enabled() {
    let fixture = Fixture::new();
    let record = install_fake_happy(&fixture);
    let output = fixture
        .command()
        .args([
            "start",
            fixture.repo.to_str().unwrap(),
            "happy-off",
            "--here",
            "--agent",
            "happy-claude",
        ])
        .env("HAPPY_RECORD", &record)
        .output()
        .unwrap();
    assert!(!output.status.success());
    // The refusal comes before any work is created.
    assert_eq!(fixture.ok(&["list"]), serde_json::json!([]));
}

#[test]
fn happy_sessions_launch_detached_tracked_and_stop_with_the_workspace() {
    let fixture = Fixture::with_config(Some(HAPPY_ENABLED));
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
                    "start",
                    fixture.repo.to_str().unwrap(),
                    &name,
                    "--here",
                    "--agent",
                    "happy-codex",
                    "--",
                    "--yolo",
                ])
                .env("HAPPY_RECORD", &record)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            let mut lines = output
                .stdout
                .split(|b| *b == b'\n')
                .filter(|l| !l.is_empty());
            let workspace: Value = serde_json::from_slice(lines.next().unwrap()).unwrap();
            assert_eq!(workspace["name"], name);
            let launch: Value = serde_json::from_slice(lines.next().unwrap()).unwrap();
            assert!(lines.next().is_none());
            assert_eq!(launch["happy_daemon_recorded"], false);
            // Without Happy credentials, the guidance message is saved instead.
            let prompt_file = launch["prompt_file"].as_str().unwrap();
            assert!(
                fs::read_to_string(prompt_file)
                    .unwrap()
                    .starts_with(GUIDANCE_START)
            );
            let config: toml::Value = toml::from_str(
                &fs::read_to_string(fixture.root.path().join(".codex/config.toml")).unwrap(),
            )
            .unwrap();
            let repository = fixture.ok(&["repo", "ls"])[0].clone();
            let directory = Path::new(repository["workspaces_dir"].as_str().unwrap());
            assert_eq!(
                config["projects"][directory.to_str().unwrap()]["trust_level"].as_str(),
                Some("trusted")
            );
            launch
        } else {
            let workspace = fixture.add(&name);
            fixture.ok(&[
                "acquire",
                "port",
                "web",
                "--workspace",
                &name,
                "--reason",
                "server",
            ]);
            fs::create_dir_all(state_file.parent().unwrap()).unwrap();
            fs::write(&state_file, "{}").unwrap();
            let claude_config = fixture.root.path().join(".claude.json");
            fs::write(&claude_config, "{}").unwrap();
            let output = fixture
                .command()
                .args(["--json", "happy-claude", &name, "--", "--model", "test"])
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
            let repository = fixture.ok(&["repo", "ls"])[0].clone();
            let directory = Path::new(repository["workspaces_dir"].as_str().unwrap());
            assert_eq!(
                config["projects"][directory.to_str().unwrap()]["hasTrustDialogAccepted"],
                true
            );
            launch
        };
        assert_eq!(launch["agent"], agent);
        let pid = launch["pid"].as_u64().unwrap() as u32;
        let execution_id = launch["execution_id"].as_str().unwrap();
        let log = PathBuf::from(launch["log"].as_str().unwrap());
        assert!(log.starts_with(fixture.root.path().join("state/workspaces")));
        let inspection = fixture.ok(&["status", &name]);
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
        assert!(
            strip_shoal_arguments(&recorded, '\0').contains(expected_args),
            "{recorded}"
        );
        assert!(recorded.contains(&format!("execution={execution_id}\n")));
        assert!(recorded.contains(&format!("workspace={name}\n")));
        assert!(!recorded.contains("scope=\n"), "{recorded}");
        assert!(recorded.contains("stdin=eof\nstdout=notty\n"), "{recorded}");
        if agent == "claude" {
            let port = fixture.ok(&["leases", "port", "--workspace", &name])["reserved"][0]["port"]
                .as_u64()
                .unwrap();
            assert!(recorded.contains(&format!("port={port}\n")), "{recorded}");
        }
        wait_until("happy output in the log", || {
            fs::read_to_string(&log).is_ok_and(|text| text.contains("hello from happy"))
        });
        let text = fs::read_to_string(&log).unwrap();
        assert!(text.contains("happy stderr"), "{text}");
        assert!(process_alive(pid));

        fixture.ok(&[operation, &name]);
        wait_until("happy to exit", || !process_alive(pid));
        if operation == "stop" {
            assert_eq!(
                fixture.ok(&["status", &name])["executions"],
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
    let fixture = Fixture::with_config(Some(HAPPY_ENABLED));
    let config_dir = fixture.root.path().join(".config/shoal");
    fs::create_dir_all(config_dir.join("rules")).unwrap();
    fs::write(config_dir.join("rules/worker.md"), "General {workspace}").unwrap();
    let record = install_fake_happy(&fixture);
    let title = "Fix API timeout";
    let body = "Keep the connection alive.";
    fixture.script_github(&github_issue(34, title, body));
    fixture.add_github_origin();
    for agent in ["happy-claude", "happy-codex"] {
        let output = fixture
            .command()
            .args([
                "--json",
                "start",
                fixture.repo.to_str().unwrap(),
                "--base",
                "HEAD",
                "--issue",
                "34",
                "--here",
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
        assert_eq!(name, "34-fix-api-timeout");
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
            assert_eq!(args[6], "--append-system-prompt");
            assert_eq!(after_guidance(args[7]), "General 34-fix-api-timeout");
            assert_eq!(after_temp_dirs(&args[8..])[..2], ["--model", "test"]);
            assert_eq!(launch["prompt_file"], Value::Null);
            assert_eq!(launch["prompt_delivered"], true, "{launch}");
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
            let prompt = after_guidance(&fs::read_to_string(&prompt_file).unwrap());
            assert!(
                prompt.starts_with("General 34-fix-api-timeout\n\n"),
                "{prompt}"
            );
            for value in [title, body, "https://github.com/team/project/issues/34"] {
                assert!(prompt.contains(value), "{prompt}");
            }
            assert_eq!(launch["prompt_delivered"], false);
            assert_eq!(launch["happy_session_id"], Value::Null);
            // The user is told where the prompt was saved.
            assert!(stderr.contains(prompt_file.to_str().unwrap()), "{stderr}");
            assert!(recorded.contains("reconnect=|||||\n"), "{recorded}");
        }
        let pid = launch["pid"].as_u64().unwrap() as u32;
        fixture.ok(&["rm", &name, "--yes"]);
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
    let fixture = Fixture::with_config(Some(HAPPY_ENABLED));
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
            .args(["--json", "happy-codex", &name, "--", "--yolo"])
            .env("HAPPY_RECORD", &record)
            .env("HAPPY_SERVER_URL", &server.url)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let launch: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(launch["prompt_delivered"], true, "{launch}");
        let session_id = launch["happy_session_id"].as_str().unwrap().to_owned();
        assert!(session_id.starts_with("session-"));
        let message = fs::read_to_string(launch["prompt_file"].as_str().unwrap()).unwrap();
        // Happy's Codex takes Shoal's guidance as its first message.
        assert!(message.starts_with(GUIDANCE_START), "{message}");
        let plaintext = serde_json::json!({
            "role": "user",
            "content": {"type": "text", "text": message},
            "meta": {"sentFrom": "shoal"},
        })
        .to_string()
        .len();

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

    // A wrong token fails delivery softly.
    fs::write(
        happy_home.join("access.key"),
        r#"{"token": "wrong", "secret": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="}"#,
    )
    .unwrap();
    fixture.add("unseeded");
    let output = fixture
        .command()
        .args(["--json", "happy-codex", "unseeded"])
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
    fixture.add("orphan");
    let launch_orphan = || {
        fixture
            .command()
            .args(["--json", "happy-codex", "orphan"])
            .env("HAPPY_SERVER_URL", &server.url)
            // Only the fixture directory and system tools: a real happy must not be found.
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    fixture.root.path().join("bin").display(),
                    fixture.system_path()
                ),
            )
            .output()
            .unwrap()
    };
    // A missing happy is refused before any session is seeded.
    let happy = fixture.root.path().join("bin/happy");
    fs::remove_file(&happy).unwrap();
    let requests = server.state()["requests"].as_array().unwrap().len();
    let output = launch_orphan();
    assert!(!output.status.success(), "{output:?}");
    assert_eq!(
        server.state()["requests"].as_array().unwrap().len(),
        requests
    );
    // An executable that cannot start fails only after seeding.
    install_test_script_with_mode(&happy, "#!/nonexistent/interpreter\n", 0o700);
    let output = launch_orphan();
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

#[test]
fn setup_verifies_after_a_concurrent_happy_chat_stops() {
    let fixture = Fixture::with_config(Some(HAPPY_ENABLED));
    let record = install_fake_happy(&fixture);
    install_test_script(
        fixture.repo.join("setup.sh"),
        r#"#!/bin/sh
touch "$HOME/setup-started"
i=0
while [ ! -e "$HOME/setup-release" ]; do
  [ "$i" -lt 3000 ] || exit 99
  sleep 0.1
  i=$((i + 1))
done
"#,
    );
    commit_resource_config(&fixture.repo, "setup.command = 'setup.sh'\n");
    let setup = fixture
        .command()
        .args([
            "--json",
            "start",
            fixture.repo.to_str().unwrap(),
            "concurrent",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_until("setup to start before the chat", || {
        fixture.root.path().join("setup-started").exists()
    });

    let started = fixture
        .command()
        .args([
            "--json",
            "start",
            "--chat",
            fixture.repo.to_str().unwrap(),
            "--agent",
            "happy-claude",
        ])
        .env("HAPPY_RECORD", &record)
        .output()
        .unwrap();
    assert!(started.status.success(), "{started:?}");
    let started: Value = serde_json::from_slice(&started.stdout).unwrap();
    wait_until("the Happy fixture to publish its launch", || {
        record.exists()
    });
    let stopped = Command::new("kill")
        .arg(started["chat"]["pid"].to_string())
        .status()
        .unwrap();
    assert!(stopped.success());
    wait_until("the daemon to record the chat stopped", || {
        fixture.ok(&["list", "--chats"])[0]["restore"] == "resume"
    });
    fs::write(fixture.root.path().join("setup-release"), "").unwrap();
    let output = setup.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let workspace: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(workspace["state"], "ready");
}

/// A detached chat's Git asks Shoal's helper for the agent token of a host
/// with one, after clearing the user's helpers for it.
#[test]
fn happy_chats_get_the_agent_token_helper_for_git() {
    let fixture = Fixture::with_config(Some(&format!(
        "{HAPPY_ENABLED}[machine.forges.\"git.example.com\"]\ntype = \"forgejo\"\nagent_token = \"agenttoken\"\n"
    )));
    let record = install_fake_happy(&fixture);
    let repo = fixture.repo.to_str().unwrap();

    let output = fixture
        .command()
        .args(["--json", "start", "--chat", repo, "--agent", "happy-claude"])
        .env("HAPPY_RECORD", &record)
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    wait_until("fake happy record", || record.exists());
    let recorded = fs::read_to_string(&record).unwrap();
    let git_config = recorded
        .split_once("git_config=")
        .unwrap()
        .1
        .trim_end_matches('\n');
    let helpers: Vec<_> = git_config
        .split('\0')
        .filter_map(|entry| entry.strip_prefix("credential.https://git.example.com.helper="))
        .collect();
    assert_eq!(helpers.len(), 2, "{recorded}");
    assert_eq!(helpers[0], "");
    assert!(
        helpers[1].ends_with(" internal git-credential"),
        "{recorded}"
    );
}

#[test]
fn happy_chats_run_detached_in_the_checkout_while_their_session_lives() {
    let fixture = Fixture::with_config(Some(HAPPY_ENABLED));
    let record = install_fake_happy(&fixture);
    let repo = fixture.repo.to_str().unwrap();
    let checkout = fs::canonicalize(&fixture.repo).unwrap();
    // Started from a Rex block, a detached chat still runs elsewhere.
    let rex_bin = fixture.root.path().join("rex-bin");
    install_test_script(rex_bin.join("rex"), "#!/bin/sh\nexit 9\n");
    let chat = |agent: &str| {
        let _ = fs::remove_file(&record);
        let output = fixture
            .command()
            .args([
                "--json", "start", "--chat", repo, "--agent", agent, "--", "--model", "test",
            ])
            .env("HAPPY_RECORD", &record)
            .env("REX_SESSION", "session:caller")
            .env("REX_BLOCK", "block:caller")
            .env("REX_BIN_DIR", &rex_bin)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let started: Value = serde_json::from_slice(&output.stdout).unwrap();
        wait_until("fake happy record", || record.exists());
        (started, fs::read_to_string(&record).unwrap())
    };
    let args = |recorded: &str| -> Vec<String> {
        recorded
            .split_once("args=")
            .unwrap()
            .1
            .split_once("\nscope=")
            .unwrap()
            .0
            .split('\0')
            .filter(|arg| !arg.is_empty())
            .map(str::to_owned)
            .collect()
    };

    // Happy's Claude takes the instructions as arguments and waits.
    let (started, recorded) = chat("happy-claude");
    let chat_record = &started["chat"];
    assert_eq!(chat_record["agent"], "happy-claude");
    assert_eq!(chat_record.get("origin"), None, "{chat_record}");
    assert_eq!(chat_record["name"], CHAT_NAME);
    assert!(
        recorded.contains(&format!("cwd={}\n", checkout.display())),
        "{recorded}"
    );
    assert!(recorded.contains(&format!("chat={}\n", chat_record["id"].as_str().unwrap())));
    assert!(recorded.contains("stdin=eof\nstdout=notty"), "{recorded}");
    let claude = args(&recorded);
    assert_eq!(
        claude[..5],
        [
            "claude",
            "--happy-starting-mode",
            "remote",
            "--started-by",
            "daemon"
        ]
    );
    assert_eq!(claude[5], "--append-system-prompt");
    assert!(
        claude[6].contains(checkout.to_str().unwrap()),
        "{}",
        claude[6]
    );
    assert_eq!(after_temp_dirs(&claude[7..]), ["--model", "test"]);
    assert_eq!(started["prompt_file"], Value::Null);
    // The chat lasts while the session runs, not the command that started it.
    let pid = chat_record["pid"].as_u64().unwrap();
    assert_eq!(
        fixture.ok(&["list", "--chats"]),
        serde_json::json!([chat_record])
    );
    let log = PathBuf::from(started["log"].as_str().unwrap());
    wait_until("the session's output", || {
        fs::read_to_string(&log).is_ok_and(|output| output.contains("hello from happy"))
    });

    // Happy's Codex takes them as its first message, saved here when Happy
    // cannot deliver it.
    let (started, recorded) = chat("happy-codex");
    assert_eq!(started["chat"]["name"], format!("{CHAT_NAME}-2"));
    assert_eq!(
        args(&recorded),
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
    let message = fs::read_to_string(started["prompt_file"].as_str().unwrap()).unwrap();
    assert!(message.contains(checkout.to_str().unwrap()), "{message}");
    assert_eq!(started["prompt_delivered"], false);

    for pid in [pid, started["chat"]["pid"].as_u64().unwrap()] {
        Command::new("kill").arg(pid.to_string()).status().unwrap();
    }
    wait_until("the chats to stop", || {
        let chats = fixture.ok(&["list", "--chats"]);
        let chats = chats.as_array().unwrap();
        !chats.is_empty() && chats.iter().all(|chat| chat["restore"] == "resume")
    });
}

/// The launch report of a start whose agent Happy took over, its last
/// line, as opposed to a queue entry.
fn happy_launch(output: &Output) -> Value {
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let launch: Value = serde_json::from_str(stdout.lines().last().unwrap()).unwrap();
    assert!(launch.get("happy_daemon_recorded").is_some(), "{stdout}");
    launch
}

/// Without a terminal, an agent Happy runs leaves the call at once anyway,
/// so its start hands it over here, needing no host to start it in the
/// background, also once the work it waits for is complete.
#[test]
fn a_start_without_a_terminal_hands_a_happy_agent_over_here() {
    let fixture = Fixture::with_config(Some(&format!("{UNLIMITED_AGENTS}{HAPPY_ENABLED}")));
    let record = install_fake_happy(&fixture);
    let path = fixture.restricted_path(&fixture.root.path().join("bin"));
    let repo = fixture.repo.to_str().unwrap();
    let start = |args: &[&str]| {
        fixture
            .command()
            .args(["--json", "start", repo])
            .args(args)
            .args(["--agent", "happy-codex"])
            .env("HAPPY_RECORD", &record)
            .env("PATH", &path)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };
    happy_launch(&start(&["placed"]));
    fixture.add("finished");
    fixture.ok(&["done", "finished"]);
    happy_launch(&start(&["next", "--after", "finished"]));
    fixture.ok(&["stop", "placed"]);
    fixture.ok(&["stop", "next"]);
}

/// Without a terminal, an agent Happy runs starts here only when the agent
/// limits admit it; with every slot taken it waits in the queue.
#[test]
fn a_start_without_a_terminal_admits_a_happy_agent_before_it_opens_here() {
    let fixture = Fixture::with_config(Some(&format!(
        "machine.max_agents = 1\n[machine.overload.disk]\nenabled = false\n{HAPPY_ENABLED}[agents.waiter]\ncommand = ['sh', '-c', 'trap \"exit 0\" TERM; while :; do sleep 1; done']\n"
    )));
    let record = install_fake_happy(&fixture);
    let repo = fixture.repo.to_str().unwrap();
    let mut first = fixture
        .command()
        .args(["start", repo, "first", "--here", "--agent", "waiter"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("the first agent start", || {
        (fixture.ok(&["list"]).as_array().unwrap().iter())
            .any(|workspace| workspace["name"] == "first" && workspace["running"] == true)
            && fixture.ok(&["queue"]) == serde_json::json!([])
    });
    let output = fixture
        .command()
        .args(["--json", "start", repo, "happy", "--agent", "happy-codex"])
        .env("HAPPY_RECORD", &record)
        .env(
            "PATH",
            fixture.restricted_path(&fixture.root.path().join("bin")),
        )
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let entry: Value = serde_json::from_str(stdout.lines().last().unwrap()).unwrap();
    assert_eq!(entry["state"], "queued", "{stdout}");
    assert!(!record.exists());
    // It waits as a launch in the workspace it set up, which it keeps.
    let status = fixture.ok(&["status", "happy"]);
    assert_eq!(
        fixture.ok(&["queue"])[0]["opens"],
        status["workspace"]["id"]
    );
    assert_eq!(
        status["cleanup_reason"]["reason"], "queued_agent",
        "{status}"
    );
    fixture.ok(&["queue", "cancel", entry["id"].as_str().unwrap()]);
    fixture.ok(&["stop", "first"]);
    first.wait().unwrap();
}
