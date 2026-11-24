use crate::events::{EventItem, EventKind};
use crate::progress::Plan;
use crate::progress::blockers::*;
use crate::stand_in::Fixture;
use crate::state::BlockerKind;
use shoal_daemon_base::blockers::{Lookup, Lookups};
use shoal_protocol::progress::Blocker;
use shoal_protocol::progress::PlanStep;
use shoal_protocol::progress::RecordedBlocker;
use shoal_protocol::space_names::SpaceSelector;

use crate::time::UnixTime;
use shoal_protocol::progress::{ProgressChange, StepState};

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
fn a_new_plan_that_reuses_a_key_drops_the_old_steps_blockers() {
    let created = |name: &str, key: &str| {
        ProgressChange::StepCreated(step(name, StepState::Pending, Some(key)))
    };
    let plan = Plan::default().changed(created("Fix", "1"));
    let plan = blocked(plan, issue(7), Some(1));
    let plan = blocked(plan, issue(8), None);
    // A new task list numbers its tasks from 1 again, for other work.
    let plan = plan.changed(created("Write the docs", "1"));
    assert_eq!(step_blockers(&plan), [("Write the docs".into(), 0)]);
    assert_eq!(plan.space_blockers().len(), 1);
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

const ISSUE: &str = "https://github.com/team/app/issues/7";
const PR: &str = "https://github.com/team/app/pull/8";
const RUN: &str = "https://github.com/team/app/actions/runs/9";

/// A new repository's workspace of each of `names`.
async fn workspaces<const N: usize>(
    manager: &crate::stand_in::Fixture,
    names: [&str; N],
) -> [crate::model::Workspace; N] {
    let repo = manager.repository("repo").await;
    let mut created = Vec::new();
    for name in names {
        let workspace = manager.create_workspace(&repo.id, name.into(), None, None, None);
        created.push(workspace.await.unwrap());
    }
    created.try_into().unwrap()
}

async fn block(
    manager: &crate::stand_in::Fixture,
    space: &crate::model::Workspace,
    blocker: Blocker,
) {
    let change = ProgressChange::AddBlocker {
        blocker,
        step: None,
        reason: "Needs it".into(),
    };
    let selector = SpaceSelector::id(&space.id);
    manager
        .spaces
        .set_progress(&**manager, &selector, change)
        .await
        .unwrap();
}

async fn blockers(
    manager: &crate::stand_in::Fixture,
    space: &crate::model::Workspace,
) -> Vec<RecordedBlocker> {
    let selector = SpaceSelector::id(&space.id);
    let progress = manager.spaces.space_progress(&selector).await.unwrap();
    progress.map_or_else(Vec::new, |progress| progress.blockers)
}

#[tokio::test]
async fn forge_blockers_clear_as_their_lookups_say_and_failures_keep_them() {
    let manager = Fixture::new().await;
    let [blocked] = workspaces(&manager, ["blocked"]).await;
    for blocker in [
        Blocker::Issue(ISSUE.parse().unwrap()),
        Blocker::Pr(PR.parse().unwrap()),
        Blocker::CiRun(RUN.parse().unwrap()),
    ] {
        block(&manager, &blocked, blocker).await;
    }
    let targets = manager.spaces.progress_forge_blockers().await.unwrap();
    assert_eq!(targets.len(), 3);
    let lookups: Lookups = [
        (
            (BlockerKind::Issue, ISSUE.into()),
            Lookup::Cleared("it closed".into()),
        ),
        (
            (BlockerKind::Pr, PR.into()),
            Lookup::Failed("forge unreachable".into()),
        ),
        (
            (BlockerKind::CiRun, RUN.into()),
            Lookup::Cleared("run 12 passed on main at abc".into()),
        ),
    ]
    .into();
    let cleared = manager
        .spaces
        .clear_progress_blockers(&*manager, &lookups)
        .await
        .unwrap();
    let how: Vec<_> = (cleared[0].cleared.iter())
        .map(|event| event.cleared.clone().unwrap_or_default())
        .collect();
    assert_eq!(how, ["it closed", "run 12 passed on main at abc"]);
    let kept = blockers(&manager, &blocked).await;
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].lookup_failed.as_deref(), Some("forge unreachable"));
    // A later lookup that succeeds forgets the failure.
    let open: Lookups = [((BlockerKind::Pr, PR.into()), Lookup::Open)].into();
    assert!(
        manager
            .spaces
            .clear_progress_blockers(&*manager, &open)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(blockers(&manager, &blocked).await[0].lookup_failed, None);

    let events: Vec<_> = (manager.space_events(None, 100).await.unwrap().into_iter())
        .filter_map(|item| match item {
            EventItem::Event(event) if event.details.kind == EventKind::BlockerCleared => {
                event.details.blocker.map(|blocker| blocker.blocker.target)
            }
            _ => None,
        })
        .collect();
    assert_eq!(events, [ISSUE, RUN]);
}
