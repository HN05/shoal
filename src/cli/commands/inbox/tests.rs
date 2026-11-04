use super::{Inbox, Item};
use crate::{cli::output::Palette, model::CheckoutSync};

#[test]
fn a_lone_sync_failure_names_its_repository() {
    let inbox = Inbox {
        items: vec![Item::SyncFailed {
            sync: CheckoutSync {
                repository_id: "r1".into(),
                synced_at: None,
                failing_since: Some(1),
                error: Some("could not refresh main\nmore".into()),
            },
        }],
        workspaces: Vec::new(),
        chats: Vec::new(),
    };
    let repositories = [("r1".to_owned(), "app".to_owned())];
    let rows = inbox.rows(&repositories, Palette::stdout(true), None);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].contains("app"), "{rows:?}");
    assert!(rows[0].contains("could not refresh main"), "{rows:?}");
    assert!(!rows[0].contains("more"), "{rows:?}");
}

#[test]
fn a_question_notes_its_recommendation_and_files() {
    let file = |id, name: &str| crate::client::files::SentFile {
        id,
        name: name.into(),
        title: None,
        path: format!("/files/{name}").into(),
    };
    let question = crate::daemon::questions::Question {
        id: 3,
        space_id: "w1".into(),
        space: "cache".into(),
        created_at: 0,
        ask: crate::daemon::questions::Ask {
            question: "Which layout?".into(),
            recommendation: "A".into(),
            reason: None,
            choices: Vec::new(),
        },
        answer: None,
        answered_at: None,
        message_id: None,
        closed_at: None,
        withdrawn: false,
        in_chat: false,
        files: vec![file(1, "a.png"), file(2, "b.png")],
    };
    assert_eq!(
        super::question_note(&question),
        "recommends A; files: a.png, b.png"
    );
}

#[test]
fn a_chat_in_a_workspace_names_the_workspace() {
    let chat: crate::model::Chat = serde_json::from_value(serde_json::json!({
        "id": "c", "repository_id": "r1", "name": "worker-chat", "agent": "claude",
        "pid": 1, "started_at": 0,
        "workspace": {"id": "w", "name": "worker", "branch": "worker"},
    }))
    .unwrap();
    let inbox = Inbox {
        items: vec![Item::ChatDone { chat: chat.clone() }],
        workspaces: Vec::new(),
        chats: vec![chat],
    };
    let rows = inbox.rows(&[], Palette::stdout(true), None);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].contains("worker-chat"), "{rows:?}");
    assert!(rows[0].ends_with("in worker"), "{rows:?}");
}
