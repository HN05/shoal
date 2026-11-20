use super::{ensure_branch_takes, names_nothing, switched_branch};
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

/// A start that names only its repository, positionally or with `--repo`,
/// leaves its source to the picker.
#[test]
fn a_repository_alone_leaves_the_source_to_the_picker() {
    let positional = Launch {
        target: Some("my-project".parse().unwrap()),
        ..Launch::default()
    };
    let flagged = Launch {
        repository: Some("my-project".into()),
        ..Launch::default()
    };
    assert!(names_nothing(&positional));
    assert!(names_nothing(&flagged));
    let linked = Launch {
        target: Some("https://forge.example/team/app/pulls/7".parse().unwrap()),
        ..Launch::default()
    };
    assert!(!names_nothing(&linked));
}
