use super::*;
use crate::client::progress::{ProgressChange, StepState};
use crate::time::UnixTime;

fn step(name: &str, state: StepState, key: Option<&str>) -> PlanStep {
    PlanStep {
        name: name.to_owned(),
        state,
        key: key.map(str::to_owned),
    }
}

fn issue(number: u64) -> RecordedBlocker {
    let url = format!("https://forge.example/team/app/issues/{number}");
    RecordedBlocker {
        kind: BlockerKind::Issue,
        target: url.clone(),
        name: url,
        reason: "Needs the fix".into(),
        lookup_failed: None,
    }
}

fn blocked(plan: Plan, blocker: RecordedBlocker, step: Option<usize>) -> Plan {
    plan.with_blocker(blocker, step).unwrap().0
}

/// The names of the blockers of each step.
fn step_blockers(plan: &Plan) -> Vec<(String, usize)> {
    let progress = plan.progress(UnixTime::EPOCH);
    (progress.steps.iter())
        .map(|step| (step.name.clone(), step.blockers.len()))
        .collect()
}

#[test]
fn a_step_blocker_follows_its_step_by_key_and_goes_with_it() {
    let created = |name: &str, key: &str| {
        ProgressChange::StepCreated(step(name, StepState::Pending, Some(key)))
    };
    let plan = Plan::default()
        .changed(created("Reproduce", "1"))
        .changed(created("Fix", "2"));
    let plan = blocked(plan, issue(7), Some(2));
    // A rename keeps the key, and the blocker with it.
    let plan = plan.changed(ProgressChange::StepChanged {
        key: "2".into(),
        name: Some("Fix the timeout".into()),
        state: None,
        removed: false,
    });
    assert_eq!(
        step_blockers(&plan),
        [("Reproduce".into(), 0), ("Fix the timeout".into(), 1)]
    );
    // Deleting the step drops its blocker.
    let plan = plan.changed(ProgressChange::StepChanged {
        key: "2".into(),
        name: None,
        state: None,
        removed: true,
    });
    assert!(plan.blockers.is_empty());
}

#[test]
fn a_step_without_a_key_keeps_its_blocker_by_name_across_a_new_list() {
    let steps = |names: &[&str]| {
        ProgressChange::Steps(
            (names.iter())
                .map(|name| step(name, StepState::Pending, None))
                .collect(),
        )
    };
    let plan = Plan::default().changed(steps(&["Build", "Ship"]));
    let plan = blocked(plan, issue(7), Some(2));
    let plan = plan.changed(steps(&["Plan", "Build", "Ship"]));
    assert_eq!(
        step_blockers(&plan),
        [("Plan".into(), 0), ("Build".into(), 0), ("Ship".into(), 1)]
    );
    let plan = plan.changed(steps(&["Plan", "Release"]));
    assert!(plan.blockers.is_empty());
}

#[test]
fn a_space_is_blocked_by_its_own_blocker_or_one_on_every_open_step() {
    let plan = Plan::default().changed(ProgressChange::Steps(vec![
        step("Reproduce", StepState::Done, None),
        step("Fix", StepState::Pending, None),
        step("Test", StepState::Pending, None),
    ]));
    let is_blocked = |plan: &Plan| plan.progress(UnixTime::EPOCH).blocked();
    assert!(!is_blocked(&plan));
    let plan = blocked(plan, issue(7), Some(2));
    // Other work remains.
    assert!(!is_blocked(&plan));
    let plan = blocked(plan, issue(8), Some(3));
    assert!(is_blocked(&plan));
    // A space blocker blocks a plan without steps too.
    assert!(is_blocked(&blocked(Plan::default(), issue(9), None)));
}

#[test]
fn adding_a_blocker_again_replaces_its_reason_and_removal_names_it() {
    let plan = blocked(Plan::default(), issue(7), None);
    let plan = blocked(
        plan,
        RecordedBlocker {
            reason: "Still open".into(),
            ..issue(7)
        },
        None,
    );
    assert_eq!(plan.space_blockers().len(), 1);
    assert_eq!(plan.space_blockers()[0].reason, "Still open");
    let named = Named {
        kind: BlockerKind::Issue,
        names: vec![issue(7).target],
    };
    // A step's removal leaves the space's blocker alone.
    assert!(plan.clone().without_blocker(&named, Some(1)).is_err());
    let (plan, removed) = plan.without_blocker(&named, None).unwrap();
    assert_eq!(removed.blocker.target, issue(7).target);
    assert!(plan.blockers.is_empty());
    assert!(plan.without_blocker(&named, None).is_err());
}
