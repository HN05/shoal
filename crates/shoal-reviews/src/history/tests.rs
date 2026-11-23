use super::*;
use crate::reviews::{Author, ReviewId, ReviewState, ReviewTarget, Severity};
use shoal_protocol::{
    changes::{ChangeEntry, ChangeKind, EntryFile},
    threads::{Outcome, Resolution, Sender},
    time::UnixTime,
};

/// The thread `1.item` of a blocker on `src/a.rs`, with a reply from its
/// holder, resolved with `outcome`.
fn finding(item: u32, outcome: Option<Outcome>) -> Thread {
    let mut thread = crate::stand_in::finding(item, "src/a.rs", Severity::Blocker, "It breaks.");
    let mut reply = thread.posts[0].clone();
    reply.side = Some(Side::Holder);
    reply.post.text = "Only on empty input.".into();
    thread.posts.push(reply);
    thread.resolution = outcome.map(|outcome| Resolution {
        outcome,
        by: Sender::Space,
        by_space: None,
        resolved_at: UnixTime::from_seconds(2),
    });
    thread
}

fn review(threads: Vec<Thread>) -> Review {
    Review {
        id: ReviewId(4),
        workspace_id: "work".into(),
        workspace: "fix-login".into(),
        target: ReviewTarget::Workspace,
        author: Author::Shoal {
            agent: "claude".into(),
            model: Default::default(),
        },
        reviewer: Some("claude".into()),
        state: ReviewState::Finished,
        failure: None,
        head: Some("0123456789abcdef".into()),
        base: Some("base".into()),
        created_at: UnixTime::from_seconds(1),
        started_at: Some(UnixTime::from_seconds(1)),
        ended_at: Some(UnixTime::from_seconds(2)),
        summary: Some("Two blockers.".into()),
        verdict: None,
        threads,
        reopened: Vec::new(),
        posted: None,
        outdated: None,
    }
}

#[test]
fn earlier_findings_carry_their_posts_outcomes_and_changes() {
    let mut fixed = finding(1, Some(Outcome::Fixed));
    fixed.changes.push(ChangeEntry {
        at: UnixTime::from_seconds(3),
        space: fixed.about().unwrap().space.clone(),
        turn: None,
        execution: None,
        numbers: vec![[1, 2]],
        files: vec![EntryFile {
            path: "src/a.rs".into(),
            kind: ChangeKind::Text,
            added: 3,
            removed: 1,
        }],
        more: None,
    });
    let reason = "The caller checks it.".to_owned();
    let reviews = [review(vec![
        fixed,
        finding(2, Some(Outcome::FalsePositive { reason })),
        finding(3, None),
    ])];
    let text = history(&[], &reviews, None, PROMPT_LIMIT);
    assert!(text.contains("0123456789ab"), "{text}");
    assert!(!text.contains("0123456789abc"), "{text}");
    assert!(
        text.contains("### Finding 1.1 blocker at src/a.rs:7: Problem 1"),
        "{text}"
    );
    assert!(text.contains("Author: Only on empty input."), "{text}");
    assert!(
        text.contains("Outcome: fixed; check it against the changes made for it: src/a.rs (+3 -1)"),
        "{text}"
    );
    assert!(
        text.contains("Outcome: false-positive: The caller checks it."),
        "{text}"
    );
    assert!(text.contains("Outcome: unresolved"), "{text}");
    assert!(history(&[], &[], None, PROMPT_LIMIT).is_empty());
}

#[test]
fn a_long_history_keeps_its_newest_part() {
    let mut long = finding(1, None);
    long.posts[0].post.text = "x".repeat(PROMPT_LIMIT);
    let reviews = [review(vec![finding(1, None), long, finding(9, None)])];
    let text = history(&[], &reviews, None, 4096);
    assert!(text.starts_with("[earlier history omitted]"));
    assert!(text.contains("Problem 9"));
    assert!(text.len() <= 4096, "{}", text.len());
}

fn false_positive(number: usize) -> FalsePositive {
    FalsePositive {
        finding: None,
        claim: format!("Claim {number}"),
        path: "ios/Info.plist".into(),
        reason: "The app extension's plist registers it.".into(),
    }
}

#[test]
fn false_positives_come_first_and_the_newest_are_kept() {
    let mut long = finding(1, None);
    long.posts[0].post.text = "x".repeat(PROMPT_LIMIT);
    let reviews = [review(vec![long, finding(9, None)])];
    let text = history(&[false_positive(1)], &reviews, None, 4096);
    assert!(
        text.starts_with(&listed(&[false_positive(1)], 4096)),
        "{text}"
    );
    assert!(
        text.contains("The app extension's plist registers it."),
        "{text}"
    );
    assert!(text.contains("Problem 9"), "{text}");
    assert!(text.len() <= 4096, "{}", text.len());

    let many: Vec<_> = (1..=2000).map(false_positive).collect();
    let text = history(&many, &reviews, None, PROMPT_LIMIT);
    let section = listed(&many, FALSE_POSITIVES_LIMIT);
    assert!(text.starts_with(&section));
    assert!(section.len() <= FALSE_POSITIVES_LIMIT, "{}", section.len());
    assert!(section.contains("## Claim 1\n"));
    assert!(!section.contains("## Claim 2000\n"));
    assert!(text.contains("Problem 9"), "{text}");
    assert!(listed(&many, 100).is_empty());
    assert!(listed(&[], PROMPT_LIMIT).is_empty());
}
