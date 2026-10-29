use super::*;
use serde_json::json;

#[test]
fn changed_files_keep_their_former_path_and_lack_lines_without_a_patch() -> Result<()> {
    let renamed = json!({
        "filename": "new.rs",
        "previous_filename": "old.rs",
        "patch": "@@ -1,2 +1,2 @@\n-a\n+b\n c",
    });
    let file = changed_file(&renamed)?;
    assert_eq!(
        (file.old_path.as_str(), file.path.as_str()),
        ("old.rs", "new.rs")
    );
    assert_eq!(file.lines.len(), 3);
    let large = changed_file(&json!({"filename": "big.lock"}))?;
    assert_eq!(large.old_path, "big.lock");
    assert!(large.lines.is_empty());
    assert!(changed_file(&json!({"patch": ""})).is_err());
    Ok(())
}

#[tokio::test]
async fn reviews_post_their_verdict_line_comments_and_head_in_one_request() -> Result<()> {
    let repo = ForgeRepo::parse("git@github.com:team/repo.git")?;
    let diff = Diff::new(vec![changed_file(&json!({
        "filename": "a.rs",
        "patch": "@@ -1,2 +1,2 @@\n-a\n+b\n c",
    }))?]);
    let comments = [crate::review::submit::LineComment {
        path: "a.rs".into(),
        line: 1,
        side: Side::Old,
        body: "Why?".into(),
    }];
    let placed = PlacedReview {
        verdict: ReviewVerdict::RequestChanges,
        body: "Fix a",
        comments: diff.place(&comments)?,
        head: Some("c0ffee"),
    };
    let submission = review(&repo, 7, &placed);
    assert!(submission.drafts.is_empty());
    let [request] = submission.requests.as_slice() else {
        panic!("one request");
    };
    assert_eq!(request.endpoint, "repos/team/repo/pulls/7/reviews");
    assert_eq!(
        request.body,
        Some(json!({
            "event": "REQUEST_CHANGES",
            "body": "Fix a",
            "commit_id": "c0ffee",
            "comments": [{"path": "a.rs", "line": 1, "side": "LEFT", "body": "Why?"}],
        }))
    );
    // Without line comments the review's own key is the only one, read
    // from its response.
    let approval = PlacedReview {
        verdict: ReviewVerdict::Approve,
        body: "",
        comments: Vec::new(),
        head: None,
    };
    let body = review(&repo, 7, &approval).requests[0].body.clone();
    assert_eq!(body, Some(json!({"event": "APPROVE", "comments": []})));
    let response = json!({"id": 80, "node_id": "PRR_80"});
    let keys = keys(&repo, &Account::default(), 7, &approval, &response).await?;
    assert_eq!(keys, ["reviews:PRR_80"]);
    Ok(())
}
