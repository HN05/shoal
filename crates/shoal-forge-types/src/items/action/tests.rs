use super::*;

#[test]
fn actions_refuse_what_the_item_cannot_take() {
    assert!(
        Action::edit(Edit::default())
            .validate(ItemKind::Pr)
            .is_err()
    );
    let base = Action::edit(Edit {
        base: Some("main".into()),
        ..Edit::default()
    });
    assert!(base.validate(ItemKind::Issue).is_err());
    assert!(base.validate(ItemKind::Pr).is_ok());
    let merge = Action::Merge {
        method: Some(MergeMethod::Merge),
        message: None,
        delete_branch: false,
        head: None,
    };
    assert!(merge.validate(ItemKind::Issue).is_err());
    assert!(
        Action::Comment {
            body: " ".into(),
            attachments: Vec::new()
        }
        .validate(ItemKind::Issue)
        .is_err()
    );
    let thread = |reply: Option<&str>, resolve| {
        Action::Thread(ThreadChange {
            comment: CommentId::new(5),
            reply: reply.map(str::to_owned),
            resolve,
        })
    };
    assert!(thread(Some("Why"), None).validate(ItemKind::Pr).is_ok());
    assert!(thread(None, Some(true)).validate(ItemKind::Pr).is_ok());
    assert!(thread(None, Some(true)).validate(ItemKind::Issue).is_err());
    assert!(thread(None, None).validate(ItemKind::Pr).is_err());
    assert!(
        thread(Some(" "), Some(true))
            .validate(ItemKind::Pr)
            .is_err()
    );
}

#[test]
fn actions_need_the_capabilities_they_use() {
    let rebase = Action::Merge {
        method: Some(MergeMethod::Rebase),
        message: None,
        delete_branch: false,
        head: None,
    };
    assert_eq!(rebase.needs(ItemKind::Pr), [Capability::RebaseMerge]);
    let auto = |method| Action::AutoMerge {
        method,
        message: None,
        delete_branch: false,
        head: None,
    };
    assert_eq!(
        auto(Some(MergeMethod::Rebase)).needs(ItemKind::Pr),
        [Capability::AutoMerge, Capability::RebaseMerge]
    );
    // The repository's default is a method the forge has.
    assert_eq!(auto(None).needs(ItemKind::Pr), [Capability::AutoMerge]);
    assert!(Action::CancelAutoMerge.validate(ItemKind::Issue).is_err());
    assert_eq!(Action::Reopen.needs(ItemKind::Pr), [Capability::ReopenPrs]);
    assert!(Action::Reopen.needs(ItemKind::Issue).is_empty());
    let labels = Action::edit(Edit {
        remove_labels: vec!["bug".into()],
        draft: Some(true),
        ..Edit::default()
    });
    assert_eq!(
        labels.needs(ItemKind::Pr),
        [Capability::Labels, Capability::DraftPrs]
    );
}

#[test]
fn reviews_need_line_comments_and_their_verdict_and_only_prs_take_them() {
    use crate::items::review::{LineComment, NewReview, ReviewVerdict, Side};
    let review = |verdict, comments: usize| {
        Action::Review(NewReview {
            verdict,
            body: "Body".into(),
            comments: (0..comments)
                .map(|_| LineComment {
                    path: "a.rs".into(),
                    line: 1,
                    side: Side::New,
                    body: "Why?".into(),
                })
                .collect(),
            commit: None,
        })
    };
    assert!(
        review(ReviewVerdict::Comment, 0)
            .needs(ItemKind::Pr)
            .is_empty()
    );
    assert_eq!(
        review(ReviewVerdict::Approve, 1).needs(ItemKind::Pr),
        [Capability::LineComments, Capability::ApprovePrs]
    );
    assert_eq!(
        review(ReviewVerdict::RequestChanges, 0).needs(ItemKind::Pr),
        [Capability::RequestChanges]
    );
    assert!(
        review(ReviewVerdict::Comment, 0)
            .validate(ItemKind::Issue)
            .is_err()
    );
}

#[test]
fn assignees_and_milestones_need_the_capability_for_the_items_kind() {
    let edit = Action::edit(Edit {
        add_assignees: vec!["sam".into()],
        milestone: Some(MilestoneChange::Clear),
        ..Edit::default()
    });
    assert_eq!(
        edit.needs(ItemKind::Issue),
        [Capability::IssueAssignees, Capability::IssueMilestones]
    );
    assert_eq!(
        edit.needs(ItemKind::Pr),
        [Capability::PrAssignees, Capability::PrMilestones]
    );
    assert!(edit.validate(ItemKind::Issue).is_ok() && edit.validate(ItemKind::Pr).is_ok());
}
