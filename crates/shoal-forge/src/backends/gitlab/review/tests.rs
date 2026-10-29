use super::*;
use crate::review::submit::{LineComment, Side};

#[test]
fn the_diff_is_the_version_at_the_head_with_the_commits_it_compares() -> Result<()> {
    let versions = json!([
        {"id": 9, "head_commit_sha": "c2"},
        {"id": 8, "head_commit_sha": "c1"},
    ]);
    assert_eq!(head_version(&versions, "c1")?, 8);
    assert!(head_version(&versions, "c3").is_err());
    let version = json!({
        "base_commit_sha": "b",
        "start_commit_sha": "s",
        "head_commit_sha": "c2",
        "diffs": [
            {"old_path": "a.rs", "new_path": "a.rs", "diff": "@@ -1 +1,2 @@\n a\n+b\n"},
            {"old_path": "gone.rs", "new_path": "gone.rs", "deleted_file": true, "diff": "@@ -1 +0,0 @@\n-x\n"},
        ],
    });
    let diff = version_diff(&version)?;
    assert_eq!(
        diff.bases,
        Some(Bases {
            base: "b".into(),
            start: "s".into()
        })
    );
    assert_eq!(diff.files.len(), 2);
    assert_eq!(diff.files[0].lines.len(), 2);
    assert_eq!(diff.files[1].path, "gone.rs");
    Ok(())
}

fn placed_review<'a>(
    verdict: ReviewVerdict,
    body: &'a str,
    diff: &'a Diff,
    comments: &'a [LineComment],
) -> Result<PlacedReview<'a>> {
    Ok(PlacedReview {
        verdict,
        body,
        comments: diff.place(comments)?,
        head: Some("h"),
        bases: diff.bases.as_ref(),
    })
}

fn line_diff() -> Result<Diff> {
    let patch = "@@ -1,2 +1,2 @@\n-a\n+b\n c";
    Ok(Diff {
        bases: Some(Bases {
            base: "b".into(),
            start: "s".into(),
        }),
        ..Diff::new(vec![FileDiff::new(
            "new.rs".into(),
            "old.rs".into(),
            patch,
        )?])
    })
}

#[test]
fn reviews_draft_each_line_then_publish_with_the_body_and_requested_changes() -> Result<()> {
    let repo = ForgeRepo::parse("https://gitlab.com/group/repo.git")?;
    let diff = line_diff()?;
    let comments = [LineComment {
        path: "new.rs".into(),
        line: 2,
        side: Side::New,
        body: "Why?".into(),
    }];
    let review = placed_review(ReviewVerdict::RequestChanges, "Fix", &diff, &comments)?;
    let submission = super::review(&repo, 7, &review)?;
    let mr = "projects/group%2Frepo/merge_requests/7";
    assert_eq!(submission.drafts[0].endpoint, format!("{mr}/draft_notes"));
    assert_eq!(
        submission.drafts[0].body,
        Some(json!({
            "note": "Why?",
            "commit_id": "h",
            "position": {
                "position_type": "text",
                "base_sha": "b",
                "start_sha": "s",
                "head_sha": "h",
                "old_path": "old.rs",
                "new_path": "new.rs",
                "old_line": 2,
                "new_line": 2,
            },
        }))
    );
    let requests: Vec<_> = (submission.requests.iter())
        .map(|request| (request.endpoint.as_str(), request.body.clone()))
        .collect();
    assert_eq!(
        requests,
        [(
            format!("{mr}/draft_notes/bulk_publish").as_str(),
            Some(json!({"note": "Fix", "reviewer_state": "requested_changes"}))
        )]
    );
    let discard = discard_draft(&repo, 7, &json!({"id": 12}))?;
    assert_eq!(
        (discard.method, discard.endpoint.as_str()),
        (HttpMethod::Delete, format!("{mr}/draft_notes/12").as_str())
    );
    Ok(())
}

#[test]
fn approvals_publish_first_and_approve_the_head() -> Result<()> {
    let repo = ForgeRepo::parse("https://gitlab.com/group/repo.git")?;
    let diff = Diff::default();
    let review = placed_review(ReviewVerdict::Approve, "Fine", &diff, &[])?;
    let submission = super::review(&repo, 7, &review)?;
    assert!(submission.drafts.is_empty());
    let requests: Vec<_> = (submission.requests.iter())
        .map(|request| (request.endpoint.as_str(), request.body.clone()))
        .collect();
    let mr = "projects/group%2Frepo/merge_requests/7";
    assert_eq!(
        requests,
        [
            (
                format!("{mr}/draft_notes/bulk_publish").as_str(),
                Some(json!({"note": "Fine"}))
            ),
            (format!("{mr}/approve").as_str(), Some(json!({"sha": "h"}))),
        ]
    );
    Ok(())
}

#[test]
fn published_notes_are_the_newest_with_each_body_of_the_review() -> Result<()> {
    let diff = line_diff()?;
    let comment = |body: &str| LineComment {
        path: "new.rs".into(),
        line: 2,
        side: Side::New,
        body: body.into(),
    };
    let comments = [comment("Same"), comment("Same")];
    let review = placed_review(ReviewVerdict::Comment, "Summary", &diff, &comments)?;
    let note = |id: u64, author: u64, body: &str| json!({"id": id, "author": {"id": author}, "body": body});
    let notes = json!([
        note(10, 2, "Same"),
        note(9, 1, "Summary"),
        note(8, 1, "Same"),
        note(7, 1, "Other"),
        note(6, 1, "Same"),
        note(5, 1, "Same"),
    ]);
    // Another reviewer's note with the same text stays theirs.
    assert_eq!(
        published(&notes, &json!(1), &review),
        ["discussion:9", "discussion:8", "discussion:6"]
    );
    Ok(())
}
