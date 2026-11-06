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
             '{0}' --json issue list --repo \"$(cat \"$HOME/repo-path\")\" \
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
    assert!(!named.status.success(), "{named:?}");
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
    let chats = fixture.ok(&["work", "--chats"]);
    assert!(!chats[0]["restore"].is_null(), "{chats}");

    // Resumed, it runs there again under its name.
    let resumed = run_agent(&fixture, &["resume", "master", "--focus"]);
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
            chats: fixture.ok(&["work", "--chats"]),
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
        assert!(output.status.success(), "{output:?}\n{transcript}");
        ran
    })
}
