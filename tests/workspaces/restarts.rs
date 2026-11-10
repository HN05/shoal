//! `shoal restart` starts an agent's session again in a new wrapper, which the
//! agent queue starts where the agent ran.
use super::chats::{start_test_chat, terminal_zmx};
use super::zmx::wait_zmx_sessions_ended;
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
        workspace: "renewed",
        path: path.clone(),
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
    assert_eq!(agent_restarted["space"], "renewed", "{restarted}");
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
            support::shoal_binary()
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

        // It answers once the chat runs again, in the zmx session it opened.
        let restarted = fixture.ok(&["restart", "talk"]);
        assert_eq!(
            restarted["restarted"],
            serde_json::json!([{
                "space": "talk",
                "agent": "claude",
                "place": {"host": "zmx", "place": "talk"},
            }]),
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

/// `shoal restart --permission` starts the agent's new wrapper with the
/// permissions it gives, even wider than its mode's, and the keys it leaves
/// out keep the values given before, as a later restart without it keeps
/// them all; an unknown key is refused before the agent stops.
#[test]
fn restart_gives_a_workspaces_agent_permissions() {
    let fixture = parent_fixture();
    let workspace = fixture.add("narrowed");
    let running = Running::start(&fixture, "narrowed", &workspace);
    let first = wait_registered_execution(&fixture, "narrowed")["id"].clone();
    let output = fixture.run(&["restart", "narrowed", "--permission", "pushes=propose"]);
    assert!(!output.status.success(), "{output:?}");
    assert!(!running.path.join("restored").exists());
    assert_eq!(wait_registered_execution(&fixture, "narrowed")["id"], first);

    let propose = serde_json::json!({"push": "propose"});
    let wider = serde_json::json!({"push": "propose", "merge": "direct"});
    let steps = [
        (&["--permission", "push=propose"][..], &propose),
        (&["--permission", "merge=direct"][..], &wider),
        (&[][..], &wider),
    ];
    for (args, given) in steps {
        let _ = fs::remove_file(running.path.join("restored"));
        fixture.ok(&[&["restart", "narrowed"][..], args].concat());
        wait_until("agent to restore", || {
            running.path.join("restored").exists()
        });
        let status = fixture.ok(&["work", "status", "narrowed"]);
        assert_eq!(
            &status["workspace"]["narrowing"], given,
            "{args:?}: {status}"
        );
    }
    let status = fixture.ok(&["work", "status", "narrowed"]);
    assert_eq!(
        status["permissions"]["merge"]["value"], "direct",
        "{status}"
    );
    drop(running);
    wait_zmx_sessions_ended(fixture.root.path());
}

/// `shoal restart --permission` restores a chat with the permissions it
/// gives, even wider than its mode's, keeping the keys it leaves out, as a
/// later restart without it keeps them all; an unknown key is refused
/// before the chat stops.
#[test]
fn restart_gives_a_chats_agent_permissions() {
    let fixture = parent_fixture();
    let home = fixture.root.path().to_owned();
    let restored = home.join("chat-restored");
    let narrowing = || fixture.ok(&["work", "--chats"])[0]["narrowing"].clone();
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
        let output = fixture.run(&["restart", "talk", "--permission", "pushes=propose"]);
        assert!(!output.status.success(), "{output:?}");
        assert_eq!(fixture.ok(&["work", "--chats"])[0]["restore"], Value::Null);
        fixture.ok(&["restart", "talk", "--permission", "push=propose"]);
        let (output, transcript) = chat.join().unwrap();
        assert!(output.status.success(), "{output:?}\n{transcript}");
    });
    wait_until("chat restored", || restored.exists());
    assert_eq!(narrowing(), serde_json::json!({"push": "propose"}));

    let wider = serde_json::json!({"push": "propose", "api_writes": "direct"});
    fs::remove_file(&restored).unwrap();
    fixture.ok(&["restart", "talk", "--permission", "api_writes=direct"]);
    wait_until("chat restored again", || restored.exists());
    assert_eq!(narrowing(), wider);
    fs::remove_file(&restored).unwrap();
    fixture.ok(&["restart", "talk"]);
    wait_until("chat restored a third time", || restored.exists());
    assert_eq!(narrowing(), wider);
    fixture.ok(&["rm", "talk", "--yes"]);
    wait_zmx_sessions_ended(&home);
}

/// A workspace's agent restarts the agent of a workspace and of a chat below
/// its own, and is refused its own and a workspace outside its tree.
#[test]
fn a_workspaces_agent_restarts_the_agents_below_its_workspace() {
    let fixture = parent_fixture();
    fixture.add("parent");
    restarts_the_agents_below(&fixture, &Parent::Workspace("parent"));
}

/// A chat's agent restarts the agents below its chat as a workspace's agent
/// does below its workspace.
#[test]
fn a_chats_agent_restarts_the_agents_below_its_chat() {
    let fixture = parent_fixture();
    let (mut agent, id) = start_test_chat(&fixture, "planning");
    let parent = Parent::chat(&fixture, "planning", id);
    restarts_the_agents_below(&fixture, &parent);
    agent.kill().unwrap();
    agent.wait().unwrap();
}

/// A workspace's agent resumes a chat below its own that its restart left
/// stopped, and is refused one the user's restart left stopped.
#[test]
fn a_workspaces_agent_resumes_the_chats_it_stopped() {
    let fixture = parent_fixture();
    fixture.add("parent");
    resumes_the_chats_it_stopped(&fixture, &Parent::Workspace("parent"));
}

/// A chat's agent resumes the chats below its chat that it stopped, as a
/// workspace's agent does.
#[test]
fn a_chats_agent_resumes_the_chats_it_stopped() {
    let fixture = parent_fixture();
    let (mut agent, id) = start_test_chat(&fixture, "planning");
    let parent = Parent::chat(&fixture, "planning", id);
    resumes_the_chats_it_stopped(&fixture, &parent);
    agent.kill().unwrap();
    agent.wait().unwrap();
}

/// A workspace's agent resumes the agent of a workspace below its own that
/// its restart left stopped, and is refused one the user's restart left
/// stopped.
#[test]
fn a_workspaces_agent_resumes_the_agents_its_restart_stopped() {
    let fixture = parent_fixture();
    fixture.add("parent");
    let parent = Parent::Workspace("parent");
    let home = fixture.root.path().to_owned();
    let zmx = home.join("bin/zmx");
    let repo = fixture.repo.to_str().unwrap();
    let child = fixture.ok(&["start", repo, "child", "--parent", parent.name()]);
    let agent = Running::start(&fixture, "child", &child);
    let restored = agent.path.join("restored");
    // No new wrapper can start, so the restart leaves the agent stopped.
    let failed_restart = |restarter: &dyn Fn(&[&str]) -> Output| {
        install_test_script(&zmx, "#!/bin/sh\nexit 1\n");
        let output = restarter(&["restart", "child"]);
        assert!(!output.status.success(), "{output:?}");
        install_test_script(&zmx, terminal_zmx());
    };

    failed_restart(&|args| fixture.run(args));
    let output = parent.run(&fixture, &["resume", "child"]);
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("ask the user"), "{output:?}");
    fixture.ok(&["resume", "child"]);
    wait_until("the user's restore", || restored.exists());
    fs::remove_file(&restored).unwrap();

    failed_restart(&|args| parent.run(&fixture, args));
    parent.ok(&fixture, &["resume", "child"]);
    wait_until("the parent's restore", || restored.exists());
    fixture.ok(&["stop", "child"]);
    drop(agent);
    wait_zmx_sessions_ended(&home);
}

/// A fixture whose workspace agents restart with `resume.sh` in their
/// worktree, whose chats run `agent.sh` in the checkout, and whose detached
/// places are zmx sessions.
fn parent_fixture() -> Fixture {
    let fixture = Fixture::with_config(Some(&format!(
        "{UNLIMITED_AGENTS}[agents.claude]\ncommand = ['sh', './agent.sh']\nresume_command = ['sh', './resume.sh']\n"
    )));
    install_test_script(fixture.root.path().join("bin/zmx"), terminal_zmx());
    fs::write(
        fixture.repo.join("agent.sh"),
        format!(
            "printf '{{\"session_id\":\"chat-session\"}}' | '{}' internal agent-state idle >/dev/null || exit 3\n\
             case \" $* \" in *' --resume '*) touch \"$HOME/chat-restored\";; *) touch \"$HOME/chat-started\";; esac\n\
             trap 'exit 0' TERM\n\
             while :; do sleep 1; done\n",
            support::shoal_binary()
        ),
    )
    .unwrap();
    fixture
}

/// The space whose agent runs a test's commands, with its scope.
enum Parent {
    /// A workspace's process, by the workspace's name.
    Workspace(&'static str),
    /// A chat's agent.
    Chat {
        name: &'static str,
        id: String,
        token: String,
    },
}

impl Parent {
    /// The chat `id`, named `name`, with the scope token its agent carries.
    fn chat(fixture: &Fixture, name: &'static str, id: String) -> Self {
        let scope = fixture.request(serde_json::json!({"chat_scope": {"chat": id}}));
        let token = scope["data"]["token"].as_str().unwrap().to_owned();
        Self::Chat { name, id, token }
    }

    fn name(&self) -> &'static str {
        match self {
            Self::Workspace(name) | Self::Chat { name, .. } => name,
        }
    }

    /// Run `shoal --json <args>` as the space's agent runs it.
    fn run(&self, fixture: &Fixture, args: &[&str]) -> Output {
        match self {
            Self::Workspace(name) => scoped_command(fixture, name, args),
            Self::Chat { id, token, .. } => fixture
                .command()
                .env("SHOAL_CHAT", id)
                .env("SHOAL_SCOPE_TOKEN", token)
                .arg("--json")
                .args(args)
                .stdin(Stdio::null())
                .output()
                .unwrap(),
        }
    }

    /// [`Self::run`], which must succeed, returning what it printed.
    fn ok(&self, fixture: &Fixture, args: &[&str]) -> Value {
        let output = self.run(fixture, args);
        assert!(output.status.success(), "{args:?}: {output:?}");
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

/// `parent` restarts the agent of the workspace `child` and of the chat
/// `talk` below it, and is refused its own space and the workspace `other`
/// outside its tree.
fn restarts_the_agents_below(fixture: &Fixture, parent: &Parent) {
    let home = fixture.root.path().to_owned();
    let repo = fixture.repo.to_str().unwrap();
    let child = fixture.ok(&["start", repo, "child", "--parent", parent.name()]);
    fixture.add("other");
    let mut agent = Running::start(fixture, "child", &child);
    for refused in [parent.name(), "other"] {
        let output = parent.run(fixture, &["restart", refused]);
        assert!(!output.status.success(), "{refused}: {output:?}");
    }

    let restarted = parent.ok(fixture, &["restart", "child"]);
    assert_eq!(restarted["restarted"][0]["space"], "child", "{restarted}");
    wait_until("agent to restore", || agent.path.join("restored").exists());
    assert!(
        try_wait_for(|| agent.agent.try_wait().unwrap()).is_some(),
        "the old wrapper still runs"
    );

    std::thread::scope(|scope| {
        let chat = scope.spawn(|| {
            fixture.interactive_command(
                fixture.command().current_dir(&fixture.repo).args([
                    "start",
                    "--chat",
                    "--here",
                    "--agent",
                    "claude",
                    "--name",
                    "talk",
                    "--parent",
                    parent.name(),
                ]),
                "",
            )
        });
        wait_until("chat started", || home.join("chat-started").exists());
        let restarted = parent.ok(fixture, &["restart", "talk"]);
        assert_eq!(restarted["restarted"][0]["space"], "talk", "{restarted}");
        let (output, transcript) = chat.join().unwrap();
        assert!(output.status.success(), "{output:?}\n{transcript}");
    });
    wait_until("chat restored", || home.join("chat-restored").exists());
    fixture.ok(&["rm", "talk", "--yes"]);
    drop(agent);
    wait_zmx_sessions_ended(&home);
}

/// A zmx that fails every command, saying so, so the daemon's log tells
/// these failures from others.
const FAILING_ZMX: &str = "#!/bin/sh\necho 'zmx fails in this test' >&2\nexit 1\n";

/// `parent` resumes the chat `talk` below it once its restart, which no
/// place could restore, left the chat stopped, but not once the user's did.
fn resumes_the_chats_it_stopped(fixture: &Fixture, parent: &Parent) {
    let home = fixture.root.path().to_owned();
    let zmx = home.join("bin/zmx");
    let restored = home.join("chat-restored");
    std::thread::scope(|scope| {
        let chat = scope.spawn(|| {
            fixture.interactive_command(
                fixture.command().current_dir(&fixture.repo).args([
                    "start",
                    "--chat",
                    "--here",
                    "--agent",
                    "claude",
                    "--name",
                    "talk",
                    "--parent",
                    parent.name(),
                ]),
                "",
            )
        });
        wait_until("chat started", || home.join("chat-started").exists());
        install_test_script(&zmx, FAILING_ZMX);
        let output = fixture.run(&["restart", "talk"]);
        assert!(!output.status.success(), "{output:?}");
        chat.join().unwrap();
    });
    install_test_script(&zmx, terminal_zmx());
    let failed_restores = || {
        let alerts = fixture.ok(&["alerts", "--all"]);
        let alerts = alerts.as_array().unwrap().clone();
        (alerts.into_iter())
            .filter(|alert| alert["kind"] == "chat_restore_failed")
            .count()
    };
    let failed = failed_restores();
    let output = parent.run(fixture, &["resume", "talk"]);
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("ask the user"), "{output:?}");
    assert!(!restored.exists());
    // The refusal comes before a place opens, so no restore failed.
    assert_eq!(failed_restores(), failed);

    fixture.ok(&["resume", "talk"]);
    wait_until("the user's restore", || restored.exists());
    fs::remove_file(&restored).unwrap();
    install_test_script(&zmx, FAILING_ZMX);
    let output = parent.run(fixture, &["restart", "talk"]);
    assert!(!output.status.success(), "{output:?}");
    install_test_script(&zmx, terminal_zmx());
    let resumed = parent.ok(fixture, &["resume", "talk"]);
    assert_eq!(resumed["chats"][0]["chat"], "talk", "{resumed}");
    wait_until("the parent's restore", || restored.exists());
    fixture.ok(&["rm", "talk", "--yes"]);
    wait_zmx_sessions_ended(&home);
}

/// The first wrapper of a workspace's agent, which the daemon stops when
/// the test ends, so a test that fails first leaves nothing running.
struct Running<'a> {
    fixture: &'a Fixture,
    workspace: &'a str,
    /// The workspace's worktree.
    path: PathBuf,
    agent: Child,
}

impl<'a> Running<'a> {
    /// Start the agent of `workspace`, as `shoal start` reported it, which
    /// `resume.sh` restores, once it runs.
    fn start(fixture: &'a Fixture, workspace: &'a str, started: &Value) -> Self {
        let path = PathBuf::from(started["path"].as_str().unwrap());
        fs::write(
            path.join("agent.sh"),
            "trap 'exit 0' TERM\ntouch agent-started\nwhile :; do sleep 1; done\n",
        )
        .unwrap();
        fs::write(
            path.join("resume.sh"),
            "trap 'exit 0' TERM\ntouch restored\nwhile :; do sleep 1; done\n",
        )
        .unwrap();
        let agent = fixture
            .command()
            .args(["claude", workspace])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let running = Self {
            fixture,
            workspace,
            path,
            agent,
        };
        wait_until("agent to start", || {
            running.path.join("agent-started").exists()
        });
        wait_registered_execution(fixture, workspace);
        running
    }
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        let _ = (self.fixture.command())
            .args(["stop", self.workspace])
            .output();
        if try_wait_for(|| self.agent.try_wait().ok().flatten()).is_none() {
            let _ = self.agent.kill();
            let _ = self.agent.wait();
        }
    }
}
