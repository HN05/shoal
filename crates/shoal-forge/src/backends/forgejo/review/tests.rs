use super::*;
use crate::review::{
    diff::FileDiff,
    submit::{LineComment, Side},
};

#[test]
fn reviews_place_each_comment_on_its_side_and_are_keyed_by_review() -> Result<()> {
    let repo = ForgeRepo::parse("https://forge.example/team/repo")?;
    let patch = "@@ -1,2 +1,2 @@\n-a\n+b\n c";
    let diff = Diff::new(vec![FileDiff::new("a.rs".into(), "a.rs".into(), patch)?]);
    let comment = |line, side| LineComment {
        path: "a.rs".into(),
        line,
        side,
        body: "Why?".into(),
    };
    let comments = [comment(1, Side::New), comment(1, Side::Old)];
    let placed = PlacedReview {
        verdict: ReviewVerdict::Approve,
        body: "",
        comments: diff.place(&comments)?,
        head: Some("c0ffee"),
        bases: None,
    };
    let submission = review(&repo, 7, &placed);
    assert_eq!(
        submission.requests[0].endpoint,
        "repos/team/repo/pulls/7/reviews"
    );
    assert_eq!(
        submission.requests[0].body,
        Some(json!({
            "event": "APPROVED",
            "body": "",
            "commit_id": "c0ffee",
            "comments": [
                {"path": "a.rs", "new_position": 1, "old_position": 0, "body": "Why?"},
                {"path": "a.rs", "new_position": 0, "old_position": 1, "body": "Why?"},
            ],
        }))
    );
    assert_eq!(keys(&json!({"id": 5}))?, ["reviews:5"]);
    assert!(keys(&json!({})).is_err());
    Ok(())
}
