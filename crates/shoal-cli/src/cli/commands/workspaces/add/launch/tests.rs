use super::switched_branch;

#[test]
fn a_picked_branch_reaches_the_daemon_as_git_switch_takes_it() {
    assert_eq!(switched_branch("refs/heads/topic"), "topic");
    assert_eq!(
        switched_branch("refs/remotes/origin/coworker/topic"),
        "coworker/topic"
    );
    assert_eq!(switched_branch("topic"), "topic");
}
