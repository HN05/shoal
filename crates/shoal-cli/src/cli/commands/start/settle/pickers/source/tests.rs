use super::{ensure_branch_takes, switched_branch};
use shoal_protocol::launches::Launch;

#[test]
fn a_picked_branch_reaches_the_daemon_with_the_remote_it_was_picked_from() {
    assert_eq!(switched_branch("refs/heads/topic"), "topic");
    assert_eq!(
        switched_branch("refs/remotes/upstream/coworker/topic"),
        "refs/remotes/upstream/coworker/topic"
    );
    assert_eq!(switched_branch("topic"), "topic");
}

/// A workspace's start without a branch begins as a chat, which takes no
/// base, so the base is refused before the agent picker asks.
#[test]
fn a_base_without_a_branch_is_refused_before_the_agent_picker() {
    let based = Launch {
        base: Some("main".into()),
        ..Launch::default()
    };
    let error = ensure_branch_takes(&based).unwrap_err();
    assert!(error.to_string().starts_with("--base "), "{error}");
    let branched = Launch {
        branch: Some("fix".into()),
        ..based
    };
    ensure_branch_takes(&branched).unwrap();
}
