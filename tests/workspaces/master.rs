//! `shoal start --mode orchestrate`: a chat without a repository, which runs
//! in `root_dir` and manages the spaces of every repository.
use super::*;

#[test]
fn orchestrate_starts_a_chat_without_a_repository_in_root_dir() {
    // The agent records where it runs, its temporary directory, its
    // process and how listing and opening a registered repository's issues
    // fail without a forge, then talks until a signal ends it.
    let fixture = Fixture::with_config(Some(
        "[agents.claude]\ncommand = ['sh', '-c', '. \"$HOME/chat-agent.sh\"', 'agent']\n",
    ));
    let home = fixture.root.path();
    fs::write(
        home.join("repo-path"),
        fixture.repo.as_os_str().as_encoded_bytes(),
    )
    .unwrap();
    fs::write(
        home.join("chat-agent.sh"),
        format!(
            "pwd -P > \"$HOME/chat-dir\"\n\
             printf %s \"$TMPDIR\" > \"$HOME/chat-tmp\"\n\
             echo $$ > \"$HOME/chat-pid\"\n\
             '{0}' --json issue ls --repo \"$(cat \"$HOME/repo-path\")\" \
                 > /dev/null 2> \"$HOME/list-error\" < /dev/null\n\
             '{0}' --json issue open --title Flaky --repo \"$(cat \"$HOME/repo-path\")\" \
                 > /dev/null 2> \"$HOME/open-error\" < /dev/null\n\
             trap 'exit 0' TERM\n\
             touch \"$HOME/chat-started\"\n\
             while :; do sleep 1; done\n",
            support::shoal_binary(),
        ),
    )
    .unwrap();
    // A repository named here is refused before anything starts.
    let named = fixture.run(&[
        "start",
        "--mode",
        "orchestrate",
        fixture.repo.to_str().unwrap(),
    ]);
    assert!(!named.status.success(), "{}", printed(&named));
    let refusal = String::from_utf8_lossy(&named.stderr);
    assert!(refusal.contains("<repository>"), "{refusal}");
    let started = run_agent(
        &fixture,
        &[
            "start",
            "--mode",
            "orchestrate",
            "--here",
            "--agent",
            "claude",
            "--name",
            "master",
        ],
    );
    let chats = &started.chats;
    assert_eq!(
        (
            &chats[0]["name"],
            &chats[0]["mode"],
            &chats[0]["repository_id"]
        ),
        (
            &Value::from("master"),
            &Value::from("orchestrate"),
            &Value::Null
        ),
        "{chats}"
    );
    let root = fs::canonicalize(home.join("shoal")).unwrap();
    assert_eq!(started.directory, root);
    let id = chats[0]["id"].as_str().unwrap();
    // It reads and opens the issues of a registered repository in that
    // repository's checkout, which has no forge here.
    for error in ["list-error", "open-error"] {
        let error = fs::read_to_string(home.join(error)).unwrap();
        assert!(error.contains("origin remote"), "{error}");
    }
    assert_eq!(started.tmp_dir, root.join(".tmp").join(id));
    let chats = fixture.ok(&["list", "--chats"]);
    assert_eq!(chats[0]["running"], false, "{chats}");

    // StartedRestores, it runs there again under its name.
    let resumed = run_agent(&fixture, &["restore", &fixture.id("master"), "--focus"]);
    assert_eq!(resumed.chats[0]["id"], id);
    assert_eq!(resumed.directory, root);
}

/// Where a chat's agent ran and the chats listed while it did.
struct Ran {
    chats: Value,
    directory: PathBuf,
    tmp_dir: PathBuf,
}

/// Run the `shoal` command `args`, whose chat's agent records where it runs,
/// in a terminal until the agent started, then end the agent.
fn run_agent(fixture: &Fixture, args: &[&str]) -> Ran {
    let home = fixture.root.path();
    let _ = fs::remove_file(home.join("chat-started"));
    std::thread::scope(|scope| {
        let chat = scope.spawn(|| {
            fixture.interactive_command(fixture.command().current_dir(home).args(args), "")
        });
        wait_until("chat started", || home.join("chat-started").exists());
        let ran = Ran {
            chats: fixture.ok(&["list", "--chats"]),
            directory: PathBuf::from(
                fs::read_to_string(home.join("chat-dir"))
                    .unwrap()
                    .trim_end(),
            ),
            tmp_dir: fs::canonicalize(fs::read_to_string(home.join("chat-tmp")).unwrap()).unwrap(),
        };
        let pid = fs::read_to_string(home.join("chat-pid")).unwrap();
        let ended = Command::new("kill")
            .args(["-TERM", pid.trim_end()])
            .status()
            .unwrap();
        assert!(ended.success());
        let (output, transcript) = chat.join().unwrap();
        assert!(
            output.status.success(),
            "{}\n{transcript}",
            printed(&output)
        );
        ran
    })
}

/// A fixture whose chat agent records where it runs and its arguments and
/// reports its session, then talks until a signal ends it.
fn session_reporting_fixture() -> Fixture {
    let fixture = Fixture::with_config(Some(
        "[agents.claude]\ncommand = ['sh', '-c', '. \"$HOME/chat-agent.sh\"', 'agent']\n",
    ));
    fs::write(
        fixture.root.path().join("chat-agent.sh"),
        format!(
            "pwd -P > \"$HOME/chat-dir\"\n\
             printf %s \"$TMPDIR\" > \"$HOME/chat-tmp\"\n\
             printf '%s\\n' \"$@\" > \"$HOME/chat-args\"\n\
             echo $$ > \"$HOME/chat-pid\"\n\
             printf '{{\"session_id\":\"master-session\"}}' | '{}' internal agent-state idle >/dev/null || exit 3\n\
             trap 'exit 0' TERM\n\
             touch \"$HOME/chat-started\"\n\
             while :; do sleep 1; done\n",
            support::shoal_binary(),
        ),
    )
    .unwrap();
    fixture
}

/// `shoal start` of the master session `master` in this terminal.
const START_MASTER: [&str; 8] = [
    "start",
    "--mode",
    "orchestrate",
    "--here",
    "--agent",
    "claude",
    "--name",
    "master",
];

#[test]
fn a_master_session_moves_and_continues_its_session_in_root_dir() {
    let fixture = session_reporting_fixture();
    let home = fixture.root.path();
    run_agent(&fixture, &START_MASTER);
    let file = home.join("master.shoal");
    let file = file.to_str().unwrap();

    let id = fixture.id("master");
    let exported = fixture.ok(&["move", "export", &id, "-o", file, "--yes"]);
    assert_eq!(
        exported["manifest"]["record"]["repository_id"],
        Value::Null,
        "{exported}"
    );
    assert_eq!(fixture.ok(&["list", "--chats"]), serde_json::json!([]));
    fixture.ok(&["restore", "--from", file, "--no-resume"]);
    let imported = run_agent(&fixture, &["restore", &id, "--focus"]);
    assert_eq!(
        (
            &imported.chats[0]["id"],
            &imported.chats[0]["repository_id"]
        ),
        (&Value::from(id.as_str()), &Value::Null),
        "{}",
        imported.chats
    );
    assert_eq!(
        imported.directory,
        fs::canonicalize(home.join("shoal")).unwrap()
    );
    let args = fs::read_to_string(home.join("chat-args")).unwrap();
    let args: Vec<&str> = args.lines().collect();
    assert!(
        args.windows(2)
            .any(|pair| pair == ["--resume", "master-session"]),
        "{args:?}"
    );
}

#[test]
fn a_root_move_takes_a_master_session_with_it() {
    let fixture = session_reporting_fixture();
    let home = fixture.root.path();
    let started = run_agent(&fixture, &START_MASTER);
    fs::write(started.tmp_dir.join("notes"), "kept").unwrap();

    fixture.ok(&["repo", "move", "~/moved", "--yes"]);

    let moved = fs::canonicalize(home.join("moved")).unwrap();
    let resumed = run_agent(&fixture, &["restore", &fixture.id("master"), "--focus"]);
    assert_eq!(resumed.directory, moved);
    assert_eq!(
        fs::read_to_string(resumed.tmp_dir.join("notes")).unwrap(),
        "kept"
    );
    assert!(resumed.tmp_dir.starts_with(&moved));
}

#[test]
fn a_master_session_creates_a_copy_for_an_issue_of_the_copied_repository() {
    let fixture = Fixture::with_config(Some("[machine.overload.disk]\nenabled = false\n"));
    fixture.add_github_origin();
    fixture.script_github(&super::forge::github_issue(
        12,
        "Clarify retries",
        "Retry twice.",
    ));
    let copied = fixture.add("copied");
    let home = fixture.root.path();
    install_test_script(home.join("bin/zmx"), PASSTHROUGH_ZMX);
    // The master session has no repository; the copied workspace names the
    // one the issue and the child are in.
    install_test_script(
        home.join("bin/claude"),
        format!(
            "#!/bin/sh\n'{}' --json create --copy {} --issue 12 > \"$HOME/created.json\" 2> \"$HOME/created.err\"\n",
            support::shoal_binary(),
            copied["id"].as_str().unwrap(),
        ),
    );
    let (output, transcript) = fixture.terminal_command(
        fixture.command().current_dir(home).args([
            "start",
            "--mode",
            "orchestrate",
            "--agent",
            "claude",
            "--name",
            "master",
        ]),
        "",
        true,
    );
    assert!(
        output.status.success(),
        "{}\n{transcript}",
        printed(&output)
    );
    let created = fs::read_to_string(home.join("created.json")).unwrap();
    let error = fs::read_to_string(home.join("created.err")).unwrap();
    let child: Value = serde_json::from_str(created.lines().next().unwrap_or_default())
        .unwrap_or_else(|_| panic!("{created}\n{error}"));
    assert_eq!(child["name"], "Clarify retries", "{child}");
    // A copy keeps its own branch, as a start's does.
    assert_eq!(child["branch"], "copied-copy", "{child}");
    assert_eq!(child["parent"]["name"], "master", "{child}");
}
