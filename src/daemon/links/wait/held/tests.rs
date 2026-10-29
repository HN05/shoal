use super::*;
use crate::{daemon::links::wait::tests::checks_at as snapshot, forge::updates::UpdateKind};

fn completed(name: &str, result: &str) -> Update {
    Update {
        url: "pr".into(),
        kind: UpdateKind::CiCompleted,
        message: format!("{name}: {result}"),
        delivery: String::new(),
        check: Some(CheckResult {
            name: name.into(),
            result: result.into(),
        }),
        workspace: None,
    }
}

fn comment() -> Update {
    Update {
        url: "pr".into(),
        kind: UpdateKind::Comment,
        message: "1 new comment".into(),
        delivery: String::new(),
        check: None,
        workspace: None,
    }
}

fn names(updates: &[Update]) -> Vec<&str> {
    updates
        .iter()
        .map(|update| {
            update
                .check
                .as_ref()
                .map_or("comment", |check| check.name.as_str())
        })
        .collect()
}

#[test]
fn passes_are_held_until_the_last_pending_check_passes() {
    let mut held = HeldPasses::default();
    let first = snapshot(
        "a",
        &[("fmt", "success"), ("lint", "pending"), ("test", "pending")],
    );
    assert!(
        held.deliver(&first, vec![completed("fmt", "success")])
            .is_empty()
    );
    let second = snapshot(
        "a",
        &[("fmt", "success"), ("lint", "skipped"), ("test", "pending")],
    );
    let delivered = held.deliver(&second, vec![completed("lint", "skipped"), comment()]);
    assert_eq!(names(&delivered), ["comment"]);
    let last = snapshot(
        "a",
        &[("fmt", "success"), ("lint", "skipped"), ("test", "success")],
    );
    let delivered = held.deliver(&last, vec![completed("test", "success")]);
    assert_eq!(names(&delivered), ["fmt", "lint", "test"]);
    assert!(held.is_empty());
}

#[test]
fn a_finished_check_that_did_not_pass_delivers_the_held_passes_at_once() {
    for result in ["failure", "cancelled"] {
        let mut held = HeldPasses::default();
        let first = snapshot(
            "a",
            &[("fmt", "success"), ("lint", "pending"), ("test", "pending")],
        );
        assert!(
            held.deliver(&first, vec![completed("fmt", "success")])
                .is_empty()
        );
        let failed = snapshot(
            "a",
            &[("fmt", "success"), ("lint", result), ("test", "pending")],
        );
        let delivered = held.deliver(&failed, vec![completed("lint", result)]);
        assert_eq!(names(&delivered), ["fmt", "lint"], "{result}");
        // Later passes wait for the rest again.
        let rerun = snapshot(
            "a",
            &[("fmt", "success"), ("lint", "pending"), ("test", "success")],
        );
        assert!(
            held.deliver(&rerun, vec![completed("test", "success")])
                .is_empty()
        );
    }
}

#[test]
fn a_new_head_drops_the_passes_held_for_the_previous_one() {
    let mut held = HeldPasses::default();
    let old = snapshot("a", &[("fmt", "success"), ("test", "pending")]);
    assert!(
        held.deliver(&old, vec![completed("fmt", "success")])
            .is_empty()
    );
    let pushed = snapshot("b", &[("fmt", "pending"), ("test", "pending")]);
    assert!(held.deliver(&pushed, Vec::new()).is_empty());
    let passed = snapshot("b", &[("fmt", "success"), ("test", "success")]);
    let delivered = held.deliver(
        &passed,
        vec![completed("fmt", "success"), completed("test", "success")],
    );
    assert_eq!(names(&delivered), ["fmt", "test"]);
}

#[test]
fn held_passes_are_delivered_once_nothing_is_pending() {
    let mut held = HeldPasses::default();
    let first = snapshot("a", &[("fmt", "success"), ("test", "pending")]);
    assert!(
        held.deliver(&first, vec![completed("fmt", "success")])
            .is_empty()
    );
    // The pending check disappeared without finishing.
    let gone = snapshot("a", &[("fmt", "success")]);
    assert_eq!(names(&held.deliver(&gone, Vec::new())), ["fmt"]);
}
