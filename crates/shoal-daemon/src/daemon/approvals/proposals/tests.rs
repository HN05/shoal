use super::*;

#[test]
fn decisions_take_only_the_details_that_apply() {
    let edit = ProposalDecision {
        title: Some("Fix".into()),
        body: Some(String::new()),
        ..ProposalDecision::default()
    };
    let duplicate = ProposalDecision {
        reason: Some("Known".into()),
        duplicate_of: Some(Duplicate::Issue("12".into())),
        ..ProposalDecision::default()
    };
    assert!(edit.validate(&Target::PrOpen, true).is_ok());
    assert!(edit.validate(&Target::IssueOpen, false).is_err());
    assert!(duplicate.validate(&Target::IssueOpen, false).is_ok());
    assert!(duplicate.validate(&Target::IssueOpen, true).is_err());
    assert!(duplicate.validate(&Target::PrOpen, false).is_err());
    let blank = ProposalDecision {
        title: Some(" ".into()),
        ..ProposalDecision::default()
    };
    assert!(blank.validate(&Target::IssueOpen, true).is_err());
}
