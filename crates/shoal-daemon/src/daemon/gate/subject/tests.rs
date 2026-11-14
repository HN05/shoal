use super::*;
use crate::daemon::approvals::{
    ApprovalRequestExt, Lifetime, PushSpecification, Target, proposals::IssueSpecification,
};
use crate::forge::items::create::NewIssue;

fn push(destination: &str, commit: &str) -> ApprovalRequest {
    let push = PushSpecification {
        remote: "origin".into(),
        url: None,
        destination: destination.into(),
        commit: Some(commit.into()),
        replaces: None,
        agent: None,
    };
    ApprovalRequest::new(
        &"space".into(),
        Target::Push,
        &push.request_name(),
        Specification::Push(push),
        Lifetime::Lease,
        None,
    )
}

fn issue(repository: Option<&str>, title: &str, body: &str) -> ApprovalRequest {
    let issue = IssueSpecification {
        issue: NewIssue {
            title: title.into(),
            body: body.into(),
            ..Default::default()
        },
        repository: repository.map(Into::into),
        link: false,
        branch: None,
        commit: None,
        agent: None,
        duplicates: Vec::new(),
        follow_up: None,
    };
    ApprovalRequest::new(
        &"space".into(),
        Target::IssueOpen,
        "issue",
        Specification::Issue(issue),
        Lifetime::Lease,
        None,
    )
}

#[test]
fn a_spaces_branch_its_issues_by_repository_and_other_refs_are_subjects_of_their_own() {
    let branch = BranchName::from("topic");
    let own = Subject::of(&push("refs/heads/topic", "a1"), Some(&branch));
    assert_eq!(own, Subject::Change);
    let other = Subject::of(&push("refs/tags/v1", "a1"), Some(&branch));
    assert_eq!(other.key(), "ref refs/tags/v1");
    assert_ne!(
        Subject::of(&issue(None, "Bug", ""), None).key(),
        Subject::of(&issue(Some("https://f/o/r"), "Bug", ""), None).key()
    );
    assert!(own.follows_commits() && !Subject::Issues(None).follows_commits());
}

#[test]
fn an_action_is_its_target_bound_to_its_commit_or_its_issues_title() {
    let pushed = action(&push("refs/heads/topic", "a1"));
    assert_eq!(pushed, action(&push("refs/heads/topic", "a1")));
    assert_ne!(pushed, action(&push("refs/heads/topic", "b2")));
    // An issue proposal is the same action whatever its description says.
    assert_eq!(
        action(&issue(None, "Bug", "one")),
        action(&issue(None, "Bug", "two"))
    );
    assert_ne!(
        action(&issue(None, "Bug", "")),
        action(&issue(None, "Other", ""))
    );
}
