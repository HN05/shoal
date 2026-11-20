use super::*;
use shoal_core::transfer::{Arrived, SshDestination};

/// A move of copies, with `--keep`, to the machine `other`.
fn copies() -> MoveOrder {
    MoveOrder {
        spaces: Vec::new(),
        destination: MoveDestination::Ssh(SshDestination {
            host: "other".into(),
            shoal: None,
            resume: true,
        }),
        recursive: true,
        keep: true,
        checkpoint: None,
    }
}

/// The copy of the chat `name`, which arrived there as `copy`.
fn copied(name: &str, copy: &str) -> SpaceMoved {
    let chat: crate::model::Chat = serde_json::from_value(serde_json::json!({
        "id": "copy", "repository_id": "repo", "name": copy, "agent": "claude",
        "started_at": 0, "session": "session",
        "workdir": {"kind": "checkout", "path": "/repo"},
    }))
    .unwrap();
    let manifest = serde_json::from_value(serde_json::json!({
        "remote_url": null,
        "record": {
            "workspace_id": "talk", "name": name, "repository_id": "repo", "cause": "manual",
            "chat": {"mode": "chat"}, "removed_at": 0, "expires_at": 0,
            "links": {"issues": [], "opened": [], "prs": [], "related": []},
        },
    }));
    SpaceMoved {
        id: "talk".into(),
        name: name.into(),
        manifest: manifest.unwrap(),
        file: None,
        removal: None,
        arrived: Some(Arrived {
            space: chat.into(),
            queued: Vec::new(),
        }),
        warnings: Vec::new(),
    }
}

#[test]
fn a_copy_under_another_name_is_named() {
    let renamed = done(&copies(), &copied("talk", "talk-2")).unwrap();
    assert!(renamed.contains("as talk-2"), "{renamed}");
    let same = done(&copies(), &copied("talk", "talk")).unwrap();
    assert!(!same.contains(" as "), "{same}");
}
