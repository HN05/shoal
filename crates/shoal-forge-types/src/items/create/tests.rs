use super::*;

#[test]
fn new_items_need_the_assignee_and_milestone_capabilities_of_their_kind() {
    let issue = NewIssue {
        assignees: vec!["kim".into()],
        milestone: Some("v2".into()),
        ..NewIssue::default()
    };
    assert_eq!(
        issue.needs(),
        [Capability::IssueAssignees, Capability::IssueMilestones]
    );
    let pull = NewPull {
        assignees: vec!["kim".into()],
        ..NewPull::default()
    };
    assert_eq!(pull.needs(), [Capability::PrAssignees]);
}
