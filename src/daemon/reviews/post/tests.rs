use super::*;
use crate::reviews::{FindingId, ReviewState};

fn finding(number: i64, path: &str, severity: Severity) -> Finding {
    Finding {
        id: FindingId { review: 9, number },
        severity,
        path: path.into(),
        line: 7,
        title: format!("Problem {number}"),
        body: "It breaks.".into(),
        replies: Vec::new(),
        outcome: None,
        check: None,
    }
}

fn review(findings: Vec<Finding>) -> Review {
    Review {
        id: 9,
        workspace_id: "work".into(),
        workspace: "fix-login".into(),
        target: ReviewTarget::Pr {
            url: "https://forge.example/team/repo/pulls/4".into(),
            head: "fix-login".into(),
            base: "main".into(),
            post: true,
        },
        agent: "codex".into(),
        reviewer: Some("claude".into()),
        state: ReviewState::Finished,
        failure: None,
        head: Some("0123456789abcdef".into()),
        base: Some("base".into()),
        created_at: 1,
        started_at: None,
        ended_at: None,
        summary: Some("One blocker.".into()),
        verdict: Some(crate::reviews::Verdict::RequestChanges),
        findings,
        posted: None,
        outdated: None,
    }
}

#[test]
fn findings_on_diff_lines_become_line_comments_and_the_rest_go_in_the_body() {
    let review = review(vec![
        finding(1, "src/a.rs", Severity::Blocker),
        finding(2, "README.md", Severity::Nit),
    ]);
    let posted = new_review(&review, |finding| finding.path == "src/a.rs", None);
    // A verdict of any kind posts as a comment.
    assert_eq!(posted.verdict, ReviewVerdict::Comment);
    assert_eq!(posted.comments.len(), 1);
    assert_eq!(posted.comments[0].path, "src/a.rs");
    assert_eq!(posted.comments[0].line, 7);
    assert_eq!(
        posted.comments[0].body,
        "**Blocker** — Problem 1\n\nIt breaks."
    );
    assert!(posted.body.starts_with(
        "One blocker.\n\nFindings outside the diff:\n\n**Nit** `README.md:7` — Problem 2"
    ));
    assert!(
        posted
            .body
            .contains("_Reviewed by claude: 1 blocker, 1 nit._")
    );
    assert!(posted.body.ends_with(&format!("_{AUTOMATED_NOTE}_")));
    assert!(!posted.body.contains("newer commits"));
    // Line comments are bound to the commit they were made on.
    assert_eq!(posted.commit.as_deref(), Some("0123456789abcdef"));
    posted.validate().unwrap();
}

#[test]
fn a_review_of_a_moved_head_puts_every_finding_in_the_body() {
    let review = review(vec![finding(1, "src/a.rs", Severity::Blocker)]);
    let posted = new_review(&review, |_| false, Some("0123456789abcdef"));
    assert!(posted.comments.is_empty());
    assert_eq!(posted.commit, None);
    assert!(
        posted
            .body
            .contains("Findings:\n\n**Blocker** `src/a.rs:7` — Problem 1")
    );
    assert!(
        posted.body.contains(
            "_Reviewed commit 0123456789ab; newer commits were pushed during the review._"
        )
    );
}

#[test]
fn a_clean_review_posts_its_summary_without_the_note() {
    let posted = new_review(&review(Vec::new()), |_| true, None);
    assert_eq!(
        posted.body,
        "One blocker.\n\n_Reviewed by claude: no findings._"
    );
    posted.validate().unwrap();
}
