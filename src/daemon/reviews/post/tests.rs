use super::*;
use crate::reviews::{FindingId, ReviewState};
use crate::time::UnixTime;

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
        model: Default::default(),
        reviewer: Some("claude".into()),
        state: ReviewState::Finished,
        failure: None,
        head: Some("0123456789abcdef".into()),
        base: Some("base".into()),
        created_at: UnixTime::from_seconds(1),
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
    assert!(posted.comments[0].body.contains("Problem 1"));
    assert!(posted.body.starts_with("One blocker."), "{}", posted.body);
    assert!(posted.body.contains("README.md:7"), "{}", posted.body);
    assert!(!posted.body.contains("Problem 1"), "{}", posted.body);
    assert!(posted.body.contains("1 blocker, 1 nit"), "{}", posted.body);
    assert!(posted.body.ends_with(&format!("_{AUTOMATED_NOTE}_")));
    assert!(!posted.body.contains("0123456789ab"), "{}", posted.body);
    // A review of the current head is bound to it.
    assert_eq!(posted.commit.as_deref(), Some("0123456789abcdef"));
    posted.validate().unwrap();
}

#[test]
fn a_review_of_a_moved_head_puts_every_finding_in_the_body() {
    let review = review(vec![finding(1, "src/a.rs", Severity::Blocker)]);
    let posted = new_review(&review, |_| false, Some("0123456789abcdef"));
    assert!(posted.comments.is_empty());
    assert_eq!(posted.commit, None);
    assert!(posted.body.contains("src/a.rs:7"), "{}", posted.body);
    assert!(posted.body.contains("0123456789ab"), "{}", posted.body);
}

#[test]
fn a_clean_review_posts_its_summary_without_the_note() {
    let posted = new_review(&review(Vec::new()), |_| true, None);
    assert_eq!(posted.commit.as_deref(), Some("0123456789abcdef"));
    assert!(posted.body.starts_with("One blocker."), "{}", posted.body);
    assert!(!posted.body.contains(AUTOMATED_NOTE), "{}", posted.body);
    posted.validate().unwrap();
}
