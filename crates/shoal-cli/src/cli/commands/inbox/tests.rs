use super::{Inbox, Item};
use crate::cli::commands::inbox::ItemExt;
use crate::time::UnixTime;
use crate::{cli::output::Palette, model::CheckoutSync};

#[test]
fn a_lone_sync_failure_names_its_repository() {
    let inbox = Inbox {
        items: vec![Item::SyncFailed {
            sync: CheckoutSync {
                repository_id: "r1".into(),
                synced_at: None,
                failing_since: Some(UnixTime::from_seconds(1)),
                error: Some("could not refresh main\nmore".into()),
            },
        }],
        workspaces: Vec::new(),
        chats: Vec::new(),
        view: None,
    };
    let repositories = [("r1".to_owned(), "app".to_owned())];
    let rows = inbox.rows(&repositories, Palette::stdout(true), None);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].contains("app"), "{rows:?}");
    assert!(rows[0].contains("could not refresh main"), "{rows:?}");
    assert!(!rows[0].contains("more"), "{rows:?}");
}

#[test]
fn a_thread_notes_its_latest_recommendation_and_files() {
    let file = |id, name: &str| shoal_protocol::files::SentFile {
        id,
        space: "w1".into(),
        name: name.into(),
        title: None,
        copy: shoal_protocol::files::KeptCopy::Here {
            path: format!("/files/{name}").into(),
        },
    };
    let thread: shoal_protocol::threads::Thread = serde_json::from_value(serde_json::json!({
        "id": 1, "space_number": 2, "item": 3, "space_id": "w1", "space": "cache",
        "kind": "question", "name": "Which layout?", "opener": "space",
        "holder": {"kind": "user"}, "created_at": 0,
        "posts": [{
            "id": 1, "created_at": 0, "side": "opener", "sender": "space",
            "text": "A or B?", "recommendation": "A",
            "files": [file(1, "a.png"), file(2, "b.png")],
        }],
    }))
    .unwrap();
    assert_eq!(
        super::thread_note(&thread),
        "recommends A; files: a.png, b.png"
    );
    let item = Item::Thread {
        thread: Box::new(thread),
    };
    assert_eq!(item.kind().0, "thread 2.3");
}

#[test]
fn a_chat_in_a_workspace_names_the_workspace() {
    let chat: crate::model::Chat = serde_json::from_value(serde_json::json!({
        "id": "c", "repository_id": "r1", "name": "worker-chat", "agent": "claude",
        "running": true, "started_at": 0,
        "workdir": {"kind": "worktree", "path": "/worker",
                    "workspace": {"id": "w", "name": "worker", "branch": "worker"}},
    }))
    .unwrap();
    let inbox = Inbox {
        items: vec![Item::ChatDone { chat: chat.clone() }],
        workspaces: Vec::new(),
        chats: vec![chat],
        view: None,
    };
    let rows = inbox.rows(&[], Palette::stdout(true), None);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].contains("worker-chat"), "{rows:?}");
    assert!(rows[0].ends_with("in worker"), "{rows:?}");
}

#[test]
fn a_chat_item_shows_its_mode_in_its_own_column() {
    let chat: crate::model::Chat = serde_json::from_value(serde_json::json!({
        "workdir": {"kind": "checkout", "path": "/repo"},
        "id": "c", "repository_id": "r1", "name": "repo-plan-7", "agent": "claude",
        "mode": "plan", "running": true, "started_at": 0,
    }))
    .unwrap();
    let inbox = Inbox {
        items: vec![Item::ChatDone { chat: chat.clone() }],
        workspaces: Vec::new(),
        chats: vec![chat],
        view: None,
    };
    let rows = inbox.rows(&[], Palette::stdout(true), None);
    assert_eq!(rows, ["finished  repo-plan-7  plan"]);
}

#[test]
fn a_running_chat_whose_agent_needs_input_is_listed() {
    let chat = |id: &str, state: &str, running: bool| -> crate::model::Chat {
        serde_json::from_value(serde_json::json!({
            "workdir": {"kind": "checkout", "path": "/repo"},
            "id": id, "repository_id": "r1", "name": id, "agent": "claude",
            "running": running, "started_at": 0,
            "agent_state": {"state": state, "since": 0},
        }))
        .unwrap()
    };
    let chats = [
        chat("asking", "waiting", true),
        chat("working", "working", true),
        chat("stopped", "waiting", false),
    ];
    let listed: Vec<String> = super::chat_items(&chats)
        .into_iter()
        .map(|item| match item {
            Item::ChatNeedsInput { chat } => chat.id.into(),
            _ => "other".into(),
        })
        .collect();
    assert_eq!(listed, ["asking"]);
}

#[test]
fn a_chat_shoal_brings_back_is_listed_asleep_with_what_wakes_it() {
    let chat = |id: &str, asleep: serde_json::Value| -> crate::model::Chat {
        serde_json::from_value(serde_json::json!({
            "workdir": {"kind": "checkout", "path": "/repo"},
            "id": id, "number": 4, "repository_id": "r1", "name": id, "agent": "claude",
            "running": true, "started_at": 0, "asleep": asleep,
        }))
        .unwrap()
    };
    let asleep = serde_json::json!({
        "reason": "critical memory pressure",
        "wakes_when": "an update arrives for it",
    });
    let chats = [
        chat("asleep", asleep),
        chat("awake", serde_json::Value::Null),
    ];
    let items = super::chat_items(&chats);
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].kind().0, "asleep");
    let text = items[0].text();
    for part in ["critical memory pressure", "an update arrives for it"] {
        assert!(text.contains(part), "{text}");
    }
    assert!(items[0].note().contains("shoal attach 4"));
}

#[test]
fn marks_wait_in_the_inbox_only_while_their_agent_is_not_working() {
    let mark = crate::model::ReviewMark {
        kind: None,
        url: None,
        created_at: UnixTime::EPOCH,
        warnings: Vec::new(),
    };
    let agent = |state: &str| serde_json::json!({"state": state, "since": 0});
    let workspace = |id: &str, state: &str| -> crate::model::Workspace {
        let mut workspace: crate::model::Workspace = serde_json::from_value(serde_json::json!({
            "workdir": {"kind": "worktree", "path": "/w", "workspace": {"id": id, "name": id, "branch": id}},
            "id": id, "repository_id": "r1", "name": id, "path": "/w", "branch": id,
            "state": "ready", "agent_state": agent(state),
            "checkout": {"state": "created", "base": {"commit": "abc"}, "git_dir": "/admin"},
        }))
        .unwrap();
        workspace.review = vec![mark.clone()];
        workspace
    };
    let chat = |id: &str, state: &str| -> crate::model::Chat {
        let mut chat: crate::model::Chat = serde_json::from_value(serde_json::json!({
            "workdir": {"kind": "checkout", "path": "/repo"},
            "id": id, "repository_id": "r1", "name": id, "agent": "claude",
            "running": true, "started_at": 0, "agent_state": agent(state),
        }))
        .unwrap();
        chat.review = vec![mark.clone()];
        chat
    };
    let workspaces = [workspace("working", "working"), workspace("idle", "idle")];
    let chats = [chat("working", "working"), chat("idle", "idle")];
    let marked = |items: Vec<Item>| -> Vec<String> {
        (items.into_iter())
            .filter_map(|item| match item {
                Item::Ready { workspace, .. } => Some(workspace.id.into()),
                Item::ChatReady { chat, .. } => Some(chat.id.into()),
                _ => None,
            })
            .collect()
    };
    let none = Default::default();
    assert_eq!(marked(super::workspace_items(&workspaces, &none)), ["idle"]);
    assert_eq!(marked(super::chat_items(&chats)), ["idle"]);
}

#[test]
fn another_machines_thread_names_its_machine() {
    let view: shoal_protocol::hub::directory::HubView = serde_json::from_value(serde_json::json!({
        "machine": "desk",
        "machines": [
            {"id": "desk", "kind": "machine", "name": "desk", "joined_at": 0,
             "connected": true, "hub": true},
            {"id": "laptop", "kind": "machine", "name": "laptop", "joined_at": 0,
             "connected": true, "hub": false},
        ],
        "spaces": [{
            "machine": "laptop", "id": "remote", "number": 4, "name": "port", "kind": "chat",
            "status": "needs-input", "reported_at": 0,
            "threads": [{
                "id": 1, "space_number": 4, "item": 1, "space_id": "remote", "space": "port",
                "kind": "question", "name": "Which database?", "opener": "space",
                "holder": {"kind": "user"}, "created_at": 0,
                "posts": [{"id": 1, "created_at": 0, "side": "opener", "sender": "space",
                           "text": "Postgres or SQLite?", "recommendation": "SQLite"}],
            }],
        }],
    }))
    .unwrap();
    let items = super::elsewhere::items(Some(&view), |_| true);
    // Each item names its own need, without its space's others.
    assert!(items.iter().all(|item| matches!(
        item,
        Item::Elsewhere { space, .. } if space.space.threads.is_empty()
    )));
    let inbox = Inbox {
        items,
        workspaces: Vec::new(),
        chats: Vec::new(),
        view: Some(view),
    };
    let rows = inbox.rows(&[], Palette::stdout(true), None);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(
        rows[0].starts_with("thread 4.1") && rows[0].contains(" port  laptop"),
        "{rows:?}"
    );
    assert!(rows[0].contains("Which database?"), "{rows:?}");
    assert!(rows[1].starts_with("needs input"), "{rows:?}");
}
