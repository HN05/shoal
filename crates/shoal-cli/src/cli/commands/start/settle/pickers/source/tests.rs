use super::switched_branch;

#[test]
fn a_picked_branch_reaches_the_daemon_with_the_remote_it_was_picked_from() {
    assert_eq!(switched_branch("refs/heads/topic"), "topic");
    assert_eq!(
        switched_branch("refs/remotes/upstream/coworker/topic"),
        "refs/remotes/upstream/coworker/topic"
    );
    assert_eq!(switched_branch("topic"), "topic");
}
