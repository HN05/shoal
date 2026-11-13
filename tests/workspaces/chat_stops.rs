//! `shoal stop` of chats: it saves them for `shoal restore`, for the user or
//! a parent's agent, and takes the chats below a stopped space.
use super::chats::start_test_chat;
use super::herdr::{herdr_calls, install_fake_herdr};
use super::restarts::{Parent, parent_fixture};
use super::zmx::wait_zmx_sessions_ended;
use super::*;

/// The user stops a running chat, which waits for the user's
/// `shoal restore`.
#[test]
fn the_user_stops_a_chat_and_resumes_it() {
    let fixture = parent_fixture();
    let home = fixture.root.path().to_owned();
    run_chat_until_stopped(&fixture, "talk", None, &[], || {
        let stopped = fixture.ok(&["stop", &fixture.id("talk")]);
        assert_eq!(stopped["stopped"], true, "{stopped}");
    });
    assert!(!chat_runs(&fixture, "talk"));

    let talk = fixture.id("talk");
    fixture.ok(&["restore", &talk]);
    wait_until("chat restored", || home.join("chat-restored").exists());
    fixture.ok(&["rm", &talk, "--yes"]);
    wait_zmx_sessions_ended(&home);
}

/// The daemon starts a chat's queued restore where its agent stopped when
/// that Herdr pane is back at its prompt, as it does a workspace's.
#[test]
fn a_queued_chat_restore_reruns_in_its_idle_pane() {
    let fixture = parent_fixture();
    install_fake_herdr(&fixture);
    let pane = [
        ("HERDR_ENV", "1"),
        ("HERDR_WORKSPACE_ID", "w1"),
        ("HERDR_TAB_ID", "w1:t5"),
        ("HERDR_PANE_ID", "w1:p5"),
    ];
    run_chat_until_stopped(&fixture, "talk", None, &pane, || {
        fixture.ok(&["stop", &fixture.id("talk")]);
    });
    fs::write(fixture.root.path().join("idle-panes"), "w1:p5\n").unwrap();

    let talk = fixture.id("talk");
    fixture.ok(&["restore", &talk]);
    let reran = || {
        let calls = fixture.root.path().join("herdr-calls");
        let calls = if calls.exists() {
            herdr_calls(&fixture)
        } else {
            Vec::new()
        };
        calls
            .into_iter()
            .find(|call| call[..3] == ["pane", "run", "w1:p5"])
    };
    wait_until("the restore in the stopped pane", || reran().is_some());
    let run = reran().unwrap();
    let restore = format!(" restore {talk} --execution ");
    assert!(run[3].contains(&restore), "{run:?}");
    assert!(run[3].contains(" --queue-entry "), "{run:?}");
    let calls = herdr_calls(&fixture);
    assert!(
        calls.iter().all(|call| call[..2] != ["tab", "create"]),
        "{calls:?}"
    );
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
    let started = fixture.root.path().join("chat-started");
    wait_until("the parent chat started", || started.exists());
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

    let stopped = fixture.ok(&["stop", &fixture.id("parent"), "--skip-children"]);
    assert_eq!(stopped["stopped"], true, "{stopped}");
    assert_eq!(running(), [true; 4]);
    let stopped = fixture.ok(&["stop", &fixture.id("below")]);
    assert_eq!(
        stopped["stopped"],
        serde_json::json!(["below", "under"]),
        "{stopped}"
    );
    assert_eq!(running(), [false, false, true, true]);
    let stopped = fixture.ok(&["stop", &fixture.id("parent")]);
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

/// Stopping the processes of several spaces refuses a stopped chat among
/// them before stopping any.
#[test]
fn stopping_processes_refuses_a_stopped_chat_first() {
    let fixture = Fixture::new();
    fixture.add("planned");
    let _chat = SleepingChat::start(&fixture, "talk", serde_json::json!({}));
    let talk = fixture.id("talk");
    fixture.ok(&["stop", &talk]);

    let planned = fixture.id("planned");
    let output = fixture.run(&["stop", "--processes", &planned, &talk]);

    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("talk stopped"), "{output:?}");
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
        let output = parent.run(fixture, &["stop", &fixture.id(refused)]);
        assert!(!output.status.success(), "{refused}: {output:?}");
    }
    assert!(chat_runs(fixture, "other"));

    run_chat_until_stopped(fixture, "talk", Some(parent.name()), &[], || {
        let stopped = parent.ok(fixture, &["stop", &fixture.id("talk")]);
        assert_eq!(stopped["stopped"], true, "{stopped}");
    });
    let talk = fixture.id("talk");
    parent.ok(fixture, &["restore", &talk]);
    wait_until("the parent's restore", || restored.exists());

    fixture.ok(&["stop", &talk]);
    parent.ok(fixture, &["stop", &talk]);
    let output = parent.run(fixture, &["restore", &talk]);
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("ask the user"), "{output:?}");
    fixture.ok(&["rm", &talk, "--yes"]);
    other.kill().unwrap();
    other.wait().unwrap();
    wait_zmx_sessions_ended(&home);
}

/// Start the chat `name` with `shoal start --mode chat --here`, below `parent`
/// when given, in a terminal with the environment `env`, and run `stop` once
/// its agent runs, which must end it.
fn run_chat_until_stopped(
    fixture: &Fixture,
    name: &str,
    parent: Option<&str>,
    env: &[(&str, &str)],
    stop: impl FnOnce(),
) {
    let started = fixture.root.path().join("chat-started");
    let _ = fs::remove_file(&started);
    let parent = parent.map(|parent| fixture.id(parent));
    let mut args = vec![
        "start", "--mode", "chat", "--here", "--agent", "claude", "--name", name,
    ];
    args.extend(parent.iter().flat_map(|parent| ["--parent", parent]));
    std::thread::scope(|scope| {
        let chat = scope.spawn(|| {
            let mut command = fixture.command();
            command
                .current_dir(&fixture.repo)
                .args(&args)
                .envs(env.iter().copied());
            fixture.interactive_command(&mut command, "")
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
    chat.unwrap_or_else(|| panic!("no chat {name}: {chats}"))["running"] == true
}

/// A chat whose agent runs until a stop ends it or the test does.
struct SleepingChat(std::process::Child);

impl SleepingChat {
    /// Start the chat `name`, placed below the space `placement`'s `parent`
    /// names, or in the worktree of the one its `workspace` names.
    fn start(fixture: &Fixture, name: &str, placement: Value) -> Self {
        let mut args = Vec::new();
        for (key, space) in placement.as_object().unwrap() {
            let flag = match key.as_str() {
                "parent" => "--parent",
                _ => "--in",
            };
            args.extend([flag.to_owned(), fixture.id(space.as_str().unwrap())]);
        }
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        Self(fixture.run_chat(name, &args).0)
    }
}

impl Drop for SleepingChat {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
