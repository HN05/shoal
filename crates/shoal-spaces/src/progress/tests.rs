use crate::progress::*;
use crate::stand_in::Fixture;
use crate::time::UnixTime;
use shoal_core::test_support::repository;
use shoal_protocol::progress::MAX_STEPS;
use shoal_protocol::progress::PlanStep;
use shoal_protocol::progress::ProgressChange;
use shoal_protocol::progress::StepState;
use shoal_protocol::space_names::SpaceSelector;

use crate::{
    events::{EventItem, EventKind},
    model::Workspace,
};
use shoal_ids::space_ids::SpaceId;

fn step(name: &str, state: StepState, key: Option<&str>) -> PlanStep {
    PlanStep {
        name: name.to_owned(),
        state,
        key: key.map(str::to_owned),
    }
}

fn names(plan: &Plan) -> Vec<(&str, StepState)> {
    (plan.steps.iter())
        .map(|step| (step.name.as_str(), step.state))
        .collect()
}

#[test]
fn tasks_change_by_key_and_a_known_key_starts_a_new_plan() {
    let created = |name: &str, key: &str| {
        ProgressChange::StepCreated(step(name, StepState::Pending, Some(key)))
    };
    let plan = Plan::default()
        .changed(created("Reproduce", "1"))
        .changed(created("Fix", "2"))
        .changed(created("Test", "3"))
        .changed(ProgressChange::StepChanged {
            key: "1".into(),
            name: None,
            state: Some(StepState::Done),
            removed: false,
        })
        .changed(ProgressChange::StepChanged {
            key: "2".into(),
            name: Some("Fix the timeout".into()),
            state: Some(StepState::Active),
            removed: false,
        })
        .changed(ProgressChange::StepChanged {
            key: "3".into(),
            name: None,
            state: None,
            removed: true,
        });
    assert_eq!(
        names(&plan),
        [
            ("Reproduce", StepState::Done),
            ("Fix the timeout", StepState::Active)
        ]
    );
    // An unknown key changes nothing.
    let unknown = ProgressChange::StepChanged {
        key: "9".into(),
        name: Some("Other".into()),
        state: None,
        removed: false,
    };
    assert_eq!(plan.clone().changed(unknown), plan);
    // A new task list numbers its tasks from the start again.
    let restarted = plan.changed(created("Plan again", "1"));
    assert_eq!(names(&restarted), [("Plan again", StepState::Pending)]);
}

#[test]
fn a_plan_keeps_its_first_steps() {
    let steps = (0..MAX_STEPS + 5)
        .map(|at| step(&format!("Step {at}"), StepState::Pending, None))
        .collect();
    let plan = Plan::default().changed(ProgressChange::Steps(steps));
    assert_eq!(plan.steps.len(), MAX_STEPS);
    assert_eq!(
        plan.steps[MAX_STEPS - 1].name,
        format!("Step {}", MAX_STEPS - 1)
    );
    let more = ProgressChange::StepCreated(step("Extra", StepState::Pending, Some("x")));
    assert_eq!(plan.clone().changed(more), plan);
}

#[test]
fn the_current_step_is_the_active_one_else_the_first_not_done() {
    let plan = |states: &[StepState]| Plan {
        status: None,
        steps: (states.iter())
            .map(|state| step("Step", *state, None))
            .collect(),
        blockers: Vec::new(),
    };
    use StepState::{Active, Done, Pending};
    let current = |states: &[StepState]| plan(states).progress(UnixTime::EPOCH).current;
    assert_eq!(current(&[Done, Pending, Active]), Some(2));
    assert_eq!(current(&[Done, Pending, Pending]), Some(1));
    assert_eq!(current(&[Done, Done]), None);
    let progress = plan(&[Done, Active, Pending]).progress(UnixTime::EPOCH);
    assert_eq!(progress.done(), 1);
}

async fn workspaces<const N: usize>(
    manager: &Fixture,
    temp: &tempfile::TempDir,
    names: [&str; N],
) -> [Workspace; N] {
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let mut created = Vec::new();
    for name in names {
        let workspace = manager
            .create_workspace(&repo.id, name.into(), None, None, None)
            .await
            .unwrap();
        created.push(workspace);
    }
    created.try_into().unwrap()
}

/// As the space and its status line.
async fn progress_events(manager: &Fixture) -> Vec<(SpaceId, Option<String>)> {
    (manager.space_events(None, 100).await.unwrap().into_iter())
        .filter_map(|item| match item {
            EventItem::Event(event) if event.details.kind == EventKind::ProgressChanged => {
                let status = event.details.progress.and_then(|progress| progress.status);
                Some((event.details.space_id, status))
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn changes_are_journaled_once_and_outlive_a_restart() {
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let [workspace] = workspaces(&manager, temp, ["progress"]).await;
    let selector = SpaceSelector::id(&workspace.id);
    let status = |text: &str| ProgressChange::Status(Some(text.to_owned()));

    assert!(
        manager
            .spaces
            .space_progress(&selector)
            .await
            .unwrap()
            .is_none()
    );
    let set = manager
        .spaces
        .set_progress(&*manager, &selector, status("Reproduced"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(set.status.as_deref(), Some("Reproduced"));
    // Setting the same value again is not a change.
    manager
        .spaces
        .set_progress(&*manager, &selector, status("Reproduced"))
        .await
        .unwrap();
    let steps = vec![step("Reproduce", StepState::Done, None)];
    manager
        .spaces
        .set_progress(&*manager, &selector, ProgressChange::Steps(steps))
        .await
        .unwrap();
    let id = workspace.id.clone();
    let reproduced = Some("Reproduced".to_owned());
    assert_eq!(
        progress_events(&manager).await,
        [(id.clone(), reproduced.clone()), (id, reproduced)]
    );
    // An invalid line is refused before anything changes.
    assert!(
        manager
            .spaces
            .set_progress(&*manager, &selector, status("two\nlines"))
            .await
            .is_err()
    );

    let reopened = manager.reopen().await;
    let loaded = reopened
        .spaces
        .workspace_by_id(&workspace.id)
        .await
        .unwrap();
    let progress = loaded.progress.unwrap();
    assert_eq!(progress.status.as_deref(), Some("Reproduced"));
    assert_eq!(
        progress.steps,
        [shoal_protocol::progress::Step {
            name: "Reproduce".into(),
            done: true,
            blockers: Vec::new(),
        }]
    );

    // Clearing everything leaves no progress to show.
    for change in [ProgressChange::Status(None), ProgressChange::Steps(vec![])] {
        reopened
            .spaces
            .set_progress(&*reopened, &selector, change)
            .await
            .unwrap();
    }
    assert!(
        reopened
            .spaces
            .space_progress(&selector)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_done_summary_sets_the_status_line_with_the_completion_it_records() {
    use crate::workspace_files::Attacher;
    use shoal_protocol::assignments::Done;
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let [workspace] = workspaces(&manager, temp, ["done"]).await;
    let selector = SpaceSelector::id(&workspace.id);
    let done = |status: &str| Done {
        summary: Some(status.to_owned()),
        ..Default::default()
    };
    manager
        .spaces
        .mark_space_done(&*manager, &selector, Done::default(), &Attacher::User)
        .await
        .unwrap();
    // A summary at the same HEAD is a change, and becomes the status line.
    manager
        .spaces
        .mark_space_done(&*manager, &selector, done("Fix merged"), &Attacher::User)
        .await
        .unwrap();
    let status = || async {
        let progress = manager.spaces.space_progress(&selector).await.unwrap();
        progress.and_then(|progress| progress.status)
    };
    assert_eq!(status().await.as_deref(), Some("Fix merged"));
    // A done refused before it records anything leaves the line alone.
    let removed = workspace.path.join(".git");
    std::fs::remove_file(&removed).unwrap();
    let refused = manager
        .spaces
        .mark_space_done(
            &*manager,
            &selector,
            done("Something else"),
            &Attacher::User,
        )
        .await;
    assert!(refused.is_err());
    assert_eq!(status().await.as_deref(), Some("Fix merged"));
}

/// As their kind and cause.
async fn blocker_events(manager: &Fixture) -> Vec<(EventKind, String)> {
    (manager.space_events(None, 100).await.unwrap().into_iter())
        .filter_map(|item| match item {
            EventItem::Event(event) => {
                let blocker = event.details.blocker?;
                Some((event.details.kind, blocker.blocker.cause()))
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn blockers_name_spaces_and_forge_items_and_are_journaled() {
    use crate::state::BlockerKind;
    use shoal_protocol::progress::Blocker;
    use shoal_protocol::progress::RecordedBlocker;
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let [blocked, api] = workspaces(&manager, temp, ["blocked", "api"]).await;
    let selector = SpaceSelector::id(&blocked.id);
    let add = |blocker: Blocker, step: Option<usize>| ProgressChange::AddBlocker {
        blocker,
        step,
        reason: "Needs it".into(),
    };
    let pr = "https://github.com/team/app/pull/8";
    let run = "https://github.com/team/app/actions/runs/9";
    for blocker in [
        Blocker::Space(api.id.to_string()),
        Blocker::Pr(pr.parse().unwrap()),
        Blocker::CiRun(run.parse().unwrap()),
    ] {
        manager
            .spaces
            .set_progress(&*manager, &selector, add(blocker, None))
            .await
            .unwrap();
    }
    // A step it does not have, and the space itself, are refused.
    let refused = [
        add(Blocker::Pr(pr.parse().unwrap()), Some(1)),
        add(Blocker::Space(blocked.id.to_string()), None),
    ];
    for change in refused {
        assert!(
            manager
                .spaces
                .set_progress(&*manager, &selector, change)
                .await
                .is_err()
        );
    }
    let progress = manager
        .spaces
        .space_progress(&selector)
        .await
        .unwrap()
        .unwrap();
    let found: Vec<_> = (progress.blockers.iter())
        .map(|blocker: &RecordedBlocker| (blocker.kind, blocker.target.as_str()))
        .collect();
    assert_eq!(
        found,
        [
            (BlockerKind::Space, api.id.as_str()),
            (BlockerKind::Pr, pr),
            (BlockerKind::CiRun, run)
        ]
    );
    assert!(progress.blocked());
    // A removal names a space by its ID, as it was added.
    let remove = ProgressChange::RemoveBlocker {
        blocker: Blocker::Space(api.id.to_string()),
        step: None,
    };
    manager
        .spaces
        .set_progress(&*manager, &selector, remove)
        .await
        .unwrap();
    assert_eq!(
        blocker_events(&manager).await,
        [
            (EventKind::BlockerAdded, "space api".into()),
            (EventKind::BlockerAdded, format!("PR {pr}")),
            (EventKind::BlockerAdded, format!("CI run {run}")),
            (EventKind::BlockerRemoved, "space api".into()),
        ]
    );
}

#[tokio::test]
async fn completion_removes_the_spaces_blockers() {
    use crate::workspace_files::Attacher;
    use shoal_protocol::assignments::Done;
    use shoal_protocol::progress::Blocker;
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let [blocked] = workspaces(&manager, temp, ["blocked"]).await;
    let selector = SpaceSelector::id(&blocked.id);
    let add = ProgressChange::AddBlocker {
        blocker: Blocker::Pr("https://github.com/team/app/pull/8".parse().unwrap()),
        step: None,
        reason: "Needs it".into(),
    };
    manager
        .spaces
        .set_progress(&*manager, &selector, add)
        .await
        .unwrap();
    manager
        .spaces
        .mark_space_done(&*manager, &selector, Done::default(), &Attacher::User)
        .await
        .unwrap();
    assert!(
        manager
            .spaces
            .space_progress(&selector)
            .await
            .unwrap()
            .is_none()
    );
    let kinds: Vec<_> = blocker_events(&manager)
        .await
        .into_iter()
        .map(|(kind, _)| kind)
        .collect();
    assert_eq!(kinds, [EventKind::BlockerAdded, EventKind::BlockerRemoved]);
}

#[tokio::test]
async fn becoming_blocked_notifies_once_until_the_space_clears() {
    use crate::notifications::AlertKind;
    use shoal_protocol::progress::Blocker;
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let [blocked] = workspaces(&manager, temp, ["blocked"]).await;
    let selector = SpaceSelector::id(&blocked.id);
    let steps = vec![
        step("Fix", StepState::Pending, None),
        step("Test", StepState::Pending, None),
    ];
    manager
        .spaces
        .set_progress(&*manager, &selector, ProgressChange::Steps(steps))
        .await
        .unwrap();
    let pr = |number: u32| {
        Blocker::Pr(
            format!("https://github.com/team/app/pull/{number}")
                .parse()
                .unwrap(),
        )
    };
    let add = |number, step| ProgressChange::AddBlocker {
        blocker: pr(number),
        step,
        reason: "Needs it".into(),
    };
    let notified = || async {
        (manager
            .spaces
            .notifications
            .list(false, 20)
            .await
            .unwrap()
            .iter())
        .filter(|notification| notification.kind == AlertKind::SpaceBlocked)
        .count()
    };
    // Other work remains.
    manager
        .spaces
        .set_progress(&*manager, &selector, add(1, Some(1)))
        .await
        .unwrap();
    assert_eq!(notified().await, 0);
    manager
        .spaces
        .set_progress(&*manager, &selector, add(2, Some(2)))
        .await
        .unwrap();
    assert_eq!(notified().await, 1);
    // Still blocked: no further notice.
    manager
        .spaces
        .set_progress(&*manager, &selector, add(3, None))
        .await
        .unwrap();
    assert_eq!(notified().await, 1);
    for (number, step) in [(1, Some(1)), (2, Some(2)), (3, None)] {
        let remove = ProgressChange::RemoveBlocker {
            blocker: pr(number),
            step,
        };
        manager
            .spaces
            .set_progress(&*manager, &selector, remove)
            .await
            .unwrap();
    }
    manager
        .spaces
        .set_progress(&*manager, &selector, add(3, None))
        .await
        .unwrap();
    assert_eq!(notified().await, 2);
}
