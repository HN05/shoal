use std::path::Path;

use super::*;
use crate::test_support;

/// A chat running in the worktree of the workspace `within`.
fn chat_in(within: &str) -> Space {
    serde_json::from_value(serde_json::json!({
        "id": "chat",
        "name": "talk",
        "repository_id": "repo",
        "workdir": {
            "kind": "worktree",
            "workspace": {"id": within, "name": within, "branch": within},
            "path": "/work",
        },
        "kind": "chat",
        "started_at": 1,
    }))
    .unwrap()
}

#[test]
fn a_hand_over_of_some_spaces_takes_the_chats_in_their_worktrees() {
    let workspace =
        |id| test_support::space(&test_support::workspace(id, "repo", id, Path::new("/w")));
    let (fix, other) = (workspace("fix"), workspace("other"));
    let only = Handing::Only([fix.id.clone()].into());
    assert!(only.takes(&fix) && only.takes(&chat_in("fix")));
    assert!(!only.takes(&other) && !only.takes(&chat_in("other")));
    let all_but = Handing::AllBut([fix.id.clone()].into());
    assert!(!all_but.takes(&fix) && !all_but.takes(&chat_in("fix")));
    assert!(all_but.takes(&other));
}
