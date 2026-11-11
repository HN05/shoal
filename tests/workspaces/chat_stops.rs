//! `shoal stop` of chats: it saves them for `shoal resume`, for the user or
//! a parent's agent, and takes the chats below a stopped space.
use super::chats::start_test_chat;
use super::restarts::{Parent, parent_fixture};
use super::zmx::wait_zmx_sessions_ended;
use super::*;

/// The user stops a running chat, which waits for the user's
/// `shoal resume`.
#[test]
fn the_user_stops_a_chat_and_resumes_it() {
    let fixture = parent_fixture();
    let home = fixture.root.path().to_owned();
    run_chat_until_stopped(&fixture, "talk", None, || {
        let stopped = fixture.ok(&["stop", "talk"]);
        assert_eq!(stopped["stopped"], true, "{stopped}");
        assert_eq!(stopped["already_stopped"], Value::Null, "{stopped}");
    });
    assert!(!chat_runs(&fixture, "talk"));

    fixture.ok(&["resume", "talk"]);
    wait_until("chat restored", || home.join("chat-restored").exists());
    fixture.ok(&["rm", "talk", "--yes"]);
    wait_zmx_sessions_ended(&home);
}

/// A workspace's agent stops a chat below its own and resumes it, and is
/// refused its own space and a chat outside its tree.
#[test]
fn a_workspaces_agent_stops_and_resumes_a_chat_below() {
    let fixture = parent_fixture();
    fixture.add("parent");
    stops_and_resumes_the_chats_below(&fixture, &Parent::Workspace("parent"));
}

/// A chat's agent stops and resumes the chats below its chat as a
/// workspace's agent does.
#[test]
fn a_chats_agent_stops_and_resumes_a_chat_below() {
    let fixture = parent_fixture();
    let (mut agent, id) = start_test_chat(&fixture, "planning");
    let parent = Parent::chat(&fixture, "planning", id);
    stops_and_resumes_the_chats_below(&fixture, &parent);
    agent.kill().unwrap();
    agent.wait().unwrap();
}

/// Stopping a space stops the chats below it, of a workspace or of a chat,
/// unless `--skip-children`; a chat in a workspace's worktree is not below
/// it.
#[test]
fn stopping_a_space_stops_the_chats_below_it() {
    let fixture = Fixture::new();
    fixture.add("parent");
    let _chats = [
        ("below", serde_json::json!({"parent": "parent"})),
        ("under", serde_json::json!({"parent": "below"})),
        ("beside", serde_json::json!({"parent": "parent"})),
        ("inside", serde_json::json!({"workspace": "parent"})),
    ]
    .map(|(name, placement)| SleepingChat::start(&fixture, name, placement));
    let running = || ["below", "under", "beside", "inside"].map(|name| chat_runs(&fixture, name));

    let stopped = fixture.ok(&["stop", "parent", "--skip-children"]);
    assert_eq!(stopped["stopped"], true, "{stopped}");
    assert_eq!(running(), [true; 4]);
    let stopped = fixture.ok(&["stop", "below"]);
    assert_eq!(
        stopped["stopped"],
        serde_json::json!(["below", "under"]),
        "{stopped}"
    );
    assert_eq!(running(), [false, false, true, true]);
    let stopped = fixture.ok(&["stop", "parent"]);
    let mut names: Vec<_> = (stopped["stopped"].as_array().unwrap().iter())
        .map(|name| name.as_str().unwrap())
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["below", "beside", "parent", "under"], "{stopped}");
    assert_eq!(running(), [false, false, false, true]);
}

/// `shoal stop --all` stops the running chats with the workspaces.
#[test]
fn stop_all_stops_running_chats() {
    let fixture = Fixture::new();
    let _chat = SleepingChat::start(&fixture, "talk", serde_json::json!({}));

    let stopped = fixture.ok(&["stop", "--all"]);

    assert_eq!(stopped["stopped"], serde_json::json!(["talk"]), "{stopped}");
    assert!(!chat_runs(&fixture, "talk"));
}

/// `parent` is refused its own space and the chat `other` outside its tree,
/// stops the chat `talk` below it and resumes it, and once the user stopped
/// `talk`, stops it again without taking it over, so its resume of it is
/// refused.
fn stops_and_resumes_the_chats_below(fixture: &Fixture, parent: &Parent) {
    let home = fixture.root.path().to_owned();
    let restored = home.join("chat-restored");
    let (mut other, _) = start_test_chat(fixture, "other");
    for refused in [parent.name(), "other"] {
        let output = parent.run(fixture, &["stop", refused]);
        assert!(!output.status.success(), "{refused}: {output:?}");
    }
    assert!(chat_runs(fixture, "other"));

    run_chat_until_stopped(fixture, "talk", Some(parent.name()), || {
        let stopped = parent.ok(fixture, &["stop", "talk"]);
        assert_eq!(stopped["stopped"], true, "{stopped}");
    });
    parent.ok(fixture, &["resume", "talk"]);
    wait_until("the parent's restore", || restored.exists());

    fixture.ok(&["stop", "talk"]);
    let stopped = parent.ok(fixture, &["stop", "talk"]);
    assert_eq!(stopped["already_stopped"], true, "{stopped}");
    let output = parent.run(fixture, &["resume", "talk"]);
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("ask the user"), "{output:?}");
    fixture.ok(&["rm", "talk", "--yes"]);
    other.kill().unwrap();
    other.wait().unwrap();
    wait_zmx_sessions_ended(&home);
}

/// Start the chat `name` with `shoal start --chat --here`, below `parent`
/// when given, and run `stop` once its agent runs, which must end it.
fn run_chat_until_stopped(
    fixture: &Fixture,
    name: &str,
    parent: Option<&str>,
    stop: impl FnOnce(),
) {
    let started = fixture.root.path().join("chat-started");
    let _ = fs::remove_file(&started);
    let mut args = vec![
        "start", "--chat", "--here", "--agent", "claude", "--name", name,
    ];
    args.extend(parent.iter().flat_map(|parent| ["--parent", *parent]));
    std::thread::scope(|scope| {
        let chat = scope.spawn(|| {
            fixture
                .interactive_command(fixture.command().current_dir(&fixture.repo).args(&args), "")
        });
        wait_until("chat started", || started.exists());
        stop();
        chat.join().unwrap();
    });
}

/// Whether the chat `name` runs, as `shoal list --chats` lists it.
fn chat_runs(fixture: &Fixture, name: &str) -> bool {
    let chats = fixture.ok(&["list", "--chats"]);
    let chat = (chats.as_array().unwrap().iter()).find(|chat| chat["name"] == name);
    chat.unwrap_or_else(|| panic!("no chat {name}: {chats}"))["restore"] == Value::Null
}

/// A chat whose agent is a `sleep`, which a thread reaps once a stop ends
/// it, so the daemon sees it gone; the chat's agent ends with the test.
struct SleepingChat {
    pid: u32,
}

impl SleepingChat {
    /// Start the chat `name`, placed as `placement`'s `parent` and
    /// `workspace` say.
    fn start(fixture: &Fixture, name: &str, placement: Value) -> Self {
        let repository = fixture.ok(&["repo", "ls"])[0].clone();
        let mut agent = support::isolated(fixture.root.path(), "sleep")
            .arg("600")
            .current_dir(fixture.root.path())
            .spawn()
            .unwrap();
        let pid = agent.id();
        std::thread::spawn(move || agent.wait());
        let mut request = serde_json::json!({
            "repository": repository["id"], "name": name, "agent": "claude", "pid": pid
        });
        request
            .as_object_mut()
            .unwrap()
            .extend(placement.as_object().unwrap().clone());
        let response = fixture.request(serde_json::json!({"start_chat": request}));
        assert_eq!(response["type"], "chat", "{response}");
        Self { pid }
    }
}

impl Drop for SleepingChat {
    fn drop(&mut self) {
        let _ = Command::new("kill").arg(self.pid.to_string()).status();
    }
}
