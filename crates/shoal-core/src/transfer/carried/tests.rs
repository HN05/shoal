use super::*;
use crate::transfer::{ManifestExt, PackagedSpaceExt};
use shoal_ids::agent_session_ids::AgentSessionId;

/// Where the space `id` sits: below `parent`, in the worktree of
/// `worktree`.
fn placed(id: &str, parent: Option<&str>, worktree: Option<&str>) -> Ids {
    (
        id.into(),
        parent.map(SpaceId::from),
        worktree.map(SpaceId::from),
    )
}

/// A space's ID with those of its parent and of the workspace whose
/// worktree it works in.
type Ids = (SpaceId, Option<SpaceId>, Option<SpaceId>);

/// The IDs [`carried_spaces`] carries with `w1` among `spaces`.
fn carried_with_w1(spaces: &[Ids], carrying: &Carrying) -> Vec<String> {
    let placed: Vec<Placed<'_>> = (spaces.iter())
        .map(|(id, parent, worktree)| Placed {
            id,
            parent: parent.as_ref(),
            worktree: worktree.as_ref(),
        })
        .collect();
    let carried = carried_spaces(&placed, &"w1".into(), carrying);
    carried.iter().map(|id| id.as_str().to_owned()).collect()
}

/// A tree below `w1`, each space listed before the spaces it waits for.
fn tree() -> Vec<Ids> {
    vec![
        // Below a chat it carries, and in the worktree of a workspace
        // below it, listed before both.
        placed("grandchild", Some("reading"), Some("w2")),
        // In its worktree, below a space that stays.
        placed("reading", Some("elsewhere"), Some("w1")),
        placed("w3", Some("w2"), None),
        placed("w2", Some("w1"), None),
        placed("chat", Some("w1"), None),
        placed("other", None, None),
    ]
}

#[test]
fn a_space_carries_every_space_below_it_and_in_its_worktrees_parents_first() {
    let carried = carried_with_w1(&tree(), &Carrying::Subtree);

    assert_eq!(carried, ["reading", "w2", "grandchild", "w3", "chat"]);
}

#[test]
fn a_selection_carries_only_its_spaces_and_those_in_their_worktrees() {
    let selected = Carrying::Selected(vec!["w2".into(), "w3".into()]);
    let carried = carried_with_w1(&tree(), &selected);
    assert_eq!(carried, ["reading", "w2", "grandchild", "w3"]);

    // A selected space below one that stays stays with it.
    let below_staying = Carrying::Selected(vec!["w3".into()]);
    assert_eq!(carried_with_w1(&tree(), &below_staying), ["reading"]);
}

#[test]
fn a_carried_space_keeps_its_session_only_where_its_agent_continues_it() {
    let removed: RemovedSpace = serde_json::from_value(serde_json::json!({
        "workspace_id": "c1", "repository_id": "r1", "name": "reading",
        "links": {"issues": [], "prs": [], "opened": [], "related": []},
        "last_sessions": [{"execution": "c1", "agent": "claude", "session": "s1",
            "origin": {"host": "herdr", "place": "tab"}, "mode": "chat"}],
        "cause": "manual", "removed_at": 1, "expires_at": 2,
        "chat": {"mode": "chat"},
    }))
    .unwrap();
    let carried = CarriedSpace::of(None, Some("m1".into()), removed);
    // The record carries no place its agent ran in.
    assert_eq!(carried.space.record.last_sessions[0].origin, None);
    let manifest = crate::transfer::Manifest {
        remote_url: None,
        space: crate::transfer::PackagedSpace {
            prerequisite: None,
            bundle: None,
            record: serde_json::from_value(serde_json::json!({
                "workspace_id": "lead", "repository_id": null, "name": "lead",
                "links": {"issues": [], "prs": [], "opened": [], "related": []},
                "cause": "manual", "removed_at": 1, "expires_at": 2,
                "chat": {"mode": "chat"},
            }))
            .unwrap(),
        },
        sessions: None,
        session_files: Vec::new(),
        copy: None,
        carried: vec![carried.clone()],
    };
    let session = |machine| {
        let sessions = manifest.carried_sessions_on(&carried, Some(machine), None);
        let chat = carried.space.record_with(sessions);
        (chat.last_sessions.first()).and_then(|last| last.session.clone())
    };

    let s1: AgentSessionId = "s1".into();
    assert_eq!(session("m1"), Some(s1));
    assert_eq!(session("m2"), None);
    assert_eq!(manifest.every_session().reported().count(), 1);
}
