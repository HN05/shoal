use super::*;
use crate::{
    daemon::{
        events::{EventItem, EventKind},
        scope::{self, Caller},
        test_support::{manager, repository},
    },
    model::Workspace,
    protocol::AgentMethod,
};

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
    };
    use StepState::{Active, Done, Pending};
    let current = |states: &[StepState]| plan(states).progress(UnixTime::EPOCH).current;
    assert_eq!(current(&[Done, Pending, Active]), Some(2));
    assert_eq!(current(&[Done, Pending, Pending]), Some(1));
    assert_eq!(current(&[Done, Done]), None);
    let progress = plan(&[Done, Active, Pending]).progress(UnixTime::EPOCH);
    assert_eq!(progress.done(), 1);
}

/// A workspace for each of `names`, in one repository.
async fn workspaces<const N: usize>(
    manager: &Manager,
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

/// The progress events of the journal, as the space and its status line.
async fn progress_events(manager: &Manager) -> Vec<(Option<SpaceId>, Option<String>)> {
    (manager
        .workspace_events(None, 100)
        .await
        .unwrap()
        .into_iter())
    .filter_map(|item| match item {
        EventItem::Event(event) if event.details.kind == EventKind::ProgressChanged => {
            let status = event.details.progress.and_then(|progress| progress.status);
            Some((event.details.workspace_id, status))
        }
        _ => None,
    })
    .collect()
}

#[tokio::test]
async fn changes_are_journaled_once_and_outlive_a_restart() {
    let (temp, manager) = manager().await;
    let [workspace] = workspaces(&manager, &temp, ["progress"]).await;
    let selector = SpaceSelector::id(&workspace.id);
    let status = |text: &str| ProgressChange::Status(Some(text.to_owned()));

    assert!(manager.space_progress(&selector).await.unwrap().is_none());
    let set = manager
        .set_progress(&selector, status("Reproduced"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(set.status.as_deref(), Some("Reproduced"));
    // Setting the same value again is not a change.
    manager
        .set_progress(&selector, status("Reproduced"))
        .await
        .unwrap();
    let steps = vec![step("Reproduce", StepState::Done, None)];
    manager
        .set_progress(&selector, ProgressChange::Steps(steps))
        .await
        .unwrap();
    let id = Some(workspace.id.clone());
    let reproduced = Some("Reproduced".to_owned());
    assert_eq!(
        progress_events(&manager).await,
        [(id.clone(), reproduced.clone()), (id, reproduced)]
    );
    // An invalid line is refused before anything changes.
    assert!(
        manager
            .set_progress(&selector, status("two\nlines"))
            .await
            .is_err()
    );

    let paths = manager.paths.clone();
    manager.store.shutdown().await;
    drop(manager);
    let reopened = Manager::open(paths).await.unwrap();
    let loaded = reopened.workspace_by_id(&workspace.id).await.unwrap();
    let progress = loaded.progress.unwrap();
    assert_eq!(progress.status.as_deref(), Some("Reproduced"));
    assert_eq!(
        progress.steps,
        [Step {
            name: "Reproduce".into(),
            done: true
        }]
    );

    // Clearing everything leaves no progress to show.
    for change in [ProgressChange::Status(None), ProgressChange::Steps(vec![])] {
        reopened.set_progress(&selector, change).await.unwrap();
    }
    assert!(reopened.space_progress(&selector).await.unwrap().is_none());
}

#[tokio::test]
async fn scoped_callers_set_only_their_own_progress_and_read_below() {
    let (temp, manager) = manager().await;
    let [own, other] = workspaces(&manager, &temp, ["own", "other"]).await;
    manager
        .executions
        .issue_scope(
            "scope".into(),
            Caller {
                space_id: own.id.clone(),
                execution: None,
            },
        )
        .await;
    let child = manager
        .split_workspace(
            &SpaceSelector::id(&own.id),
            "child".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let allowed = async |method: &mut AgentMethod| {
        scope::authorize(&manager, Some("scope"), method)
            .await
            .is_ok()
    };
    let set = |space: &Workspace| AgentMethod::SetProgress {
        space: SpaceSelector::id(&space.id),
        change: ProgressChange::Status(Some("Working".into())),
    };
    let read = |space: &Workspace| AgentMethod::Progress {
        space: SpaceSelector::id(&space.id),
    };
    assert!(allowed(&mut set(&own)).await);
    assert!(!allowed(&mut set(&child)).await);
    assert!(allowed(&mut read(&own)).await);
    assert!(allowed(&mut read(&child)).await);
    assert!(!allowed(&mut read(&other)).await);
}

#[tokio::test]
async fn a_chats_progress_is_its_own_and_goes_with_it() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(
            repo.id.clone().into(),
            "planning".into(),
            "claude".into(),
            std::process::id(),
            None,
            false,
        )
        .await
        .unwrap();
    let selector = SpaceSelector::id(&chat.id);
    manager
        .set_progress(&selector, ProgressChange::Status(Some("Drafting".into())))
        .await
        .unwrap();
    let listed = manager.chats().await.unwrap();
    let progress = listed[0].progress.as_ref().unwrap();
    assert_eq!(progress.status.as_deref(), Some("Drafting"));
    let events: Vec<_> = (manager.workspace_events(None, 100).await.unwrap())
        .into_iter()
        .filter_map(|item| match item {
            EventItem::Event(event) if event.details.kind == EventKind::ProgressChanged => {
                Some(event.details)
            }
            _ => None,
        })
        .collect();
    assert_eq!(events.len(), 1);
    assert_eq!(
        (
            events[0].workspace_id.as_deref(),
            events[0].chat_id.as_deref()
        ),
        (None, Some(chat.id.as_str()))
    );
    assert_eq!(events[0].repository_id.as_deref(), Some(repo.id.as_str()));

    let id = chat.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute("DELETE FROM chats WHERE id=?1", [&id])?;
            Ok(
                db.query_row("SELECT COUNT(*) FROM space_progress", [], |row| {
                    row.get::<_, i64>(0)
                })?,
            )
        })
        .await
        .map(|left| assert_eq!(left, 0))
        .unwrap();
}

#[tokio::test]
async fn a_done_summary_sets_the_status_line_with_the_completion_it_records() {
    use crate::{client::assignments::Done, daemon::workspace_files::Attacher};
    let (temp, manager) = manager().await;
    let [workspace] = workspaces(&manager, &temp, ["done"]).await;
    let selector = SpaceSelector::id(&workspace.id);
    let done = |status: &str| Done {
        summary: Some(status.to_owned()),
        ..Default::default()
    };
    manager
        .mark_space_done(&selector, Done::default(), &Attacher::User)
        .await
        .unwrap();
    // A summary at the same HEAD is a change, and becomes the status line.
    manager
        .mark_space_done(&selector, done("Fix merged"), &Attacher::User)
        .await
        .unwrap();
    let status = || async {
        let progress = manager.space_progress(&selector).await.unwrap();
        progress.and_then(|progress| progress.status)
    };
    assert_eq!(status().await.as_deref(), Some("Fix merged"));
    // A done refused before it records anything leaves the line alone.
    let removed = workspace.path.join(".git");
    std::fs::remove_file(&removed).unwrap();
    let refused = manager
        .mark_space_done(&selector, done("Something else"), &Attacher::User)
        .await;
    assert!(refused.is_err());
    assert_eq!(status().await.as_deref(), Some("Fix merged"));
}
