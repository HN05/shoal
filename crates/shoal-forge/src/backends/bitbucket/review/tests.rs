use super::*;
use crate::items::{
    item::{Item, ItemState},
    link::ItemKind,
};

#[test]
fn the_diff_runs_from_the_merge_base_to_the_head() -> Result<()> {
    let repo = ForgeRepo::parse("https://bitbucket.org/team/repo.git")?;
    let mut current = Current {
        item: Item {
            kind: ItemKind::Pr,
            number: 3,
            url: String::new(),
            title: "Fix".into(),
            state: ItemState::Open,
            labels: Vec::new(),
            pr: None,
        },
        node_id: None,
        head_repository: None,
        body: String::new(),
        author: None,
        head_sha: Some("abc".into()),
        base_sha: Some("def".into()),
        conflict: None,
        merges_now: false,
        reviewers: Vec::new(),
        user_ids: Default::default(),
        replaced_labels: Vec::new(),
    };
    assert_eq!(
        diff_endpoint(&repo, &current)?,
        "repositories/team/repo/diff/abc..def?topic=true"
    );
    current.base_sha = None;
    assert!(diff_endpoint(&repo, &current).is_err());
    Ok(())
}

#[test]
fn reviews_comment_on_each_line_then_post_the_body_and_the_verdict() -> Result<()> {
    use crate::review::{
        diff::FileDiff,
        submit::{LineComment, ReviewVerdict, Side},
    };
    let repo = ForgeRepo::parse("https://bitbucket.org/team/repo.git")?;
    let patch = "@@ -1,2 +1,2 @@\n-a\n+b\n c";
    let diff = Diff::new(vec![FileDiff::new("a.rs".into(), "a.rs".into(), patch)?]);
    let comment = |line, side| LineComment {
        path: "a.rs".into(),
        line,
        side,
        body: "Why?".into(),
    };
    let comments = [comment(1, Side::New), comment(1, Side::Old)];
    let review = PlacedReview {
        verdict: ReviewVerdict::RequestChanges,
        body: "Fix",
        comments: diff.place(&comments)?,
        head: Some("abc"),
        bases: None,
    };
    let requests: Vec<_> = (super::review(&repo, 3, &review).requests.into_iter())
        .map(|request| (request.endpoint, request.body))
        .collect();
    let pull = "repositories/team/repo/pullrequests/3";
    let comments = format!("{pull}/comments");
    assert_eq!(
        requests,
        [
            (
                comments.clone(),
                Some(serde_json::json!({"content": {"raw": "Why?"},
                    "inline": {"path": "a.rs", "to": 1}}))
            ),
            (
                comments.clone(),
                Some(serde_json::json!({"content": {"raw": "Why?"},
                    "inline": {"path": "a.rs", "from": 1}}))
            ),
            (
                comments,
                Some(serde_json::json!({"content": {"raw": "Fix"}}))
            ),
            (format!("{pull}/request-changes"), None),
        ]
    );
    Ok(())
}
