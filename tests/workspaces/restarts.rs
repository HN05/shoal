//! `shoal restart` starts an agent's session again in a new wrapper, which the
//! agent queue starts where the agent ran.
use super::*;

/// The old wrapper's process tree cannot start the agent, as one that lost
/// access to the workspace's volume cannot. The new wrapper, which the agent
/// queue starts in a detached zmx session, can, the old one finishes, and the
/// run names that session.
#[test]
fn restart_starts_the_agent_in_a_new_wrapper_where_the_old_one_could_not() {
    let fixture = Fixture::with_config(Some(&format!(
        "{UNLIMITED_AGENTS}[agents.claude]\ncommand = ['sh', './agent.sh']\nresume_command = ['sh', './resume.sh']\n"
    )));
    install_test_script(fixture.root.path().join("bin/zmx"), PASSTHROUGH_ZMX);
    let workspace = fixture.add("renewed");
    let path = Path::new(workspace["path"].as_str().unwrap()).to_owned();
    fs::write(
        path.join("agent.sh"),
        "trap 'exit 0' TERM\ntouch agent-started\nwhile :; do sleep 1; done\n",
    )
    .unwrap();
    fs::write(
        path.join("resume.sh"),
        "if [ -n \"$BROKEN_TREE\" ]; then exit 1; fi\n\
         trap 'exit 0' TERM\ntouch restored\nwhile :; do sleep 1; done\n",
    )
    .unwrap();
    let mut agent = Running {
        fixture: &fixture,
        agent: fixture
            .command()
            .env("BROKEN_TREE", "1")
            .args(["claude", "renewed"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    };
    wait_until("agent to start", || path.join("agent-started").exists());
    let old = wait_registered_execution(&fixture, "renewed")["id"].clone();

    let restarted = fixture.ok(&["restart", "renewed"]);
    let [agent_restarted] = restarted["restarted"].as_array().unwrap().as_slice() else {
        panic!("expected one restarted agent: {restarted}");
    };
    assert_eq!(agent_restarted["workspace"], "renewed", "{restarted}");
    assert_eq!(agent_restarted["agent"], "claude", "{restarted}");
    // The answer names the detached session the new wrapper runs in.
    assert_eq!(agent_restarted["place"]["host"], "zmx", "{restarted}");
    wait_until("agent to restore", || path.join("restored").exists());
    // The old wrapper finished, and a new one runs the agent.
    assert!(
        try_wait_for(|| agent.agent.try_wait().unwrap()).is_some(),
        "the old wrapper still runs"
    );
    let new = wait_registered_execution(&fixture, "renewed")["id"].clone();
    assert_ne!(new, old);
    // The old wrapper's end tells the user of no agent exit.
    let alerts = fixture.ok(&["alerts", "--all"]);
    let exits = alerts.as_array().into_iter().flatten();
    assert!(
        exits
            .into_iter()
            .all(|alert| alert["kind"] != "agent_exited"),
        "{alerts}"
    );
    fixture.ok(&["stop", "renewed"]);
    wait_zmx_sessions_ended(fixture.root.path());
}

/// A chat restarts as `shoal resume` restores it: the daemon saves its
/// session and stops its agent, then restores it in a zmx session, where the
/// agent continues the session.
#[test]
fn restart_restores_a_chat_in_a_new_place() {
    let fixture = Fixture::with_config(Some(
        "[agents.claude]\ncommand = ['sh', './chat-agent.sh']\n",
    ));
    let home = fixture.root.path().to_owned();
    fs::write(
        fixture.repo.join("chat-agent.sh"),
        format!(
            "printf '%s\\n' \"$@\" > \"$HOME/chat-args\"\n\
             printf '{{\"session_id\":\"chat-session\"}}' | '{}' internal agent-state idle >/dev/null || exit 3\n\
             case \" $* \" in *' --resume '*) touch \"$HOME/chat-restored\";; *) touch \"$HOME/chat-started\";; esac\n\
             trap 'exit 0' TERM\n\
             while :; do sleep 1; done\n",
            env!("CARGO_BIN_EXE_shoal")
        ),
    )
    .unwrap();
    install_test_script(home.join("bin").join("zmx"), terminal_zmx());
    std::thread::scope(|scope| {
        let chat = scope.spawn(|| {
            fixture.interactive_command(
                fixture.command().current_dir(&fixture.repo).args([
                    "start", "--chat", "--here", "--agent", "claude", "--name", "talk",
                ]),
                "",
            )
        });
        wait_until("chat started", || home.join("chat-started").exists());

        let restarted = fixture.ok(&["restart", "talk"]);
        assert_eq!(
            restarted["restarted"],
            serde_json::json!([{"workspace": "talk", "agent": "claude"}]),
            "{restarted}"
        );
        // The first agent ended, and the restored one continues its session.
        let (output, transcript) = chat.join().unwrap();
        assert!(output.status.success(), "{output:?}\n{transcript}");
    });
    wait_until("chat restored", || home.join("chat-restored").exists());
    let args = fs::read_to_string(home.join("chat-args")).unwrap();
    assert!(args.contains("--resume\nchat-session\n"), "{args}");
    let chats = fixture.ok(&["work", "--chats"]);
    assert_eq!(chats[0]["restore"], Value::Null, "{chats}");
    fixture.ok(&["rm", "talk", "--yes"]);
    wait_zmx_sessions_ended(&home);
}

/// The first wrapper of the workspace `renewed`, which the daemon stops when
/// the test ends, so a test that fails first leaves nothing running.
struct Running<'a> {
    fixture: &'a Fixture,
    agent: Child,
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        let _ = self.fixture.command().args(["stop", "renewed"]).output();
        if try_wait_for(|| self.agent.try_wait().ok().flatten()).is_none() {
            let _ = self.agent.kill();
            let _ = self.agent.wait();
        }
    }
}
