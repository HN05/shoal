use crate::daemon::scope::Access;
use shoal_ids::execution_ids::ExecutionId;
use std::{collections::BTreeMap, time::Duration};

use anyhow::Result;

use crate::daemon::agent_queue::Grant;
use crate::daemon::agents::ProtectionExt;
use crate::protocol::operations::{Ended, Failure, OperationSpec, Preparation, Report, Step};
use crate::{
    daemon::{
        manager::Manager,
        queued_starts::fake::STARTED,
        test_support::{global_config, manager, repository},
        workspace_files::Attacher,
    },
    model::{QueueAdmission, QueueEntry, QueueRequest, Repository, Workspace},
    state::{QueueKind, QueueState, WorkspaceState},
};
use shoal_protocol::assignments::Done;
use shoal_protocol::space_names::SpaceSelector;
use std::sync::Arc;

pub(crate) async fn register(
    manager: &Manager,
    temp: &tempfile::TempDir,
    name: &str,
) -> Repository {
    let checkout = repository(temp.path(), name);
    manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap()
}

/// Limit the machine to `limit` agents; host disk must not decide the test.
pub(crate) fn limit_globally(manager: &Manager, limit: u32) {
    manager.publish_config(global_config(&format!(
        "[machine]\nmax_heavy_agents = {limit}\n[machine.overload.disk]\nenabled = false"
    )));
}

/// The launch of an agent in a new workspace of `repository` named `name`.
pub(crate) async fn request(
    manager: &Manager,
    repository: &Repository,
    name: &str,
    now: bool,
) -> QueueRequest {
    launch_in(&create(manager, repository, name).await, now)
}

/// The launch of an agent in `workspace`, which the daemon starts.
pub(crate) fn launch_in(workspace: &Workspace, now: bool) -> QueueRequest {
    QueueRequest {
        source: None,
        replace: false,
        issue_labels: Vec::new(),
        narrowing: Default::default(),
        mode: None,
        name: workspace.name.clone(),
        agent: "claude".into(),
        issue_title: None,
        plan: "{}".into(),
        env: BTreeMap::from([("HERDR_ENV".into(), "1".into())]),
        now,
        space: SpaceSelector::id(&workspace.id),
        starts_in_caller: false,
        blockers: Vec::new(),
    }
}

/// Queue the launch of an agent in a new workspace of `repository` named
/// `name`.
pub(crate) async fn add(manager: &Manager, repository: &Repository, name: &str) -> QueueAdmission {
    let request = request(manager, repository, name, false).await;
    manager.agents.queue_add(manager, request).await.unwrap()
}

/// Whether `admission` admitted its launch, which the daemon starts.
pub(super) fn admitted(admission: &QueueAdmission) -> bool {
    (admission.entry.as_ref()).is_some_and(|entry| entry.state == QueueState::Starting)
}

/// Whether the daemon started `id`, passing the queuing terminal's Herdr variables.
pub(super) fn started(id: &QueueEntryId) -> bool {
    STARTED.lock().unwrap().iter().any(|(started, env)| {
        id == started && env.get("HERDR_ENV").map(String::as_str) == Some("1")
    })
}

#[tokio::test]
async fn completed_workspaces_and_registered_agents_release_starting_slots() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    let workspace = create(&manager, &repo, "first").await;
    let first = manager
        .agents
        .queue_add(&*manager, launch_in(&workspace, false))
        .await;
    let first = first.unwrap().entry.unwrap();
    let second = add(&manager, &repo, "second").await.entry.unwrap();
    manager
        .agents
        .queue_attach(&*manager, &first.id, &SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    manager.agents.dispatch_queue(&*manager).await.unwrap();
    assert!(!started(&second.id));

    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    manager.agents.dispatch_queue(&*manager).await.unwrap();
    assert!(started(&second.id));

    // Only the queued launch's own agent takes over its entry, whose
    // workspace stays known once it left the queue.
    manager
        .agents
        .consume_queue_entry(&second.id, &workspace.id)
        .await;
    assert_eq!(
        manager
            .agents
            .queue_entries(&*manager,)
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        manager.agents.started_launch(&second.id).await.unwrap(),
        None
    );
    manager
        .agents
        .consume_queue_entry(&first.id, &workspace.id)
        .await;
    // Recorded in the transaction that took the entry out, in the store,
    // so neither a poll in between nor a daemon restart misses it.
    assert_eq!(
        manager.agents.started_launch(&first.id).await.unwrap(),
        Some(workspace.id.clone())
    );
    let names: Vec<_> = manager
        .agents
        .queue_entries(&*manager)
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    assert_eq!(names, ["second"]);
}

#[tokio::test]
async fn dynamic_admission_staggers_starts() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    // Host memory and disk must not decide this test.
    manager.publish_config(global_config(
        "[machine.overload.memory]\nenabled = false\n[machine.overload.disk]\nenabled = false",
    ));
    // An idle machine starts the first agent whatever its CPU reads.
    assert!(admitted(&add(&manager, &repo, "first").await));
    // The next waits for the stagger even with room.
    let second = add(&manager, &repo, "second").await;
    assert!(!admitted(&second));
    assert_eq!(second.waiting, 1);
}

/// An agent connected in `workspace`, which protection may stop, and the
/// receiver that keeps its connection open.
pub(super) async fn connected_agent(
    manager: &Manager,
    workspace: &crate::model::Workspace,
) -> (ExecutionId, tokio::sync::watch::Receiver<bool>) {
    let started = manager
        .agents
        .begin_execution(
            manager,
            workspace.id.as_str(),
            None,
            crate::daemon::executions::ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    manager
        .agents
        .track_agent(
            manager,
            &started.plan.id,
            &shoal_protocol::executions::TrackedAgent::named("claude"),
            &workspace.id,
            &workspace.name,
            true,
            Duration::from_secs(3600),
        )
        .await;
    (started.plan.id, started.stop)
}

/// A hang guard, not a deadline: the awaited step is already admitted.
pub(super) async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(60), future)
        .await
        .expect("queue step hung")
}

pub(crate) async fn create(
    manager: &Manager,
    repository: &Repository,
    name: &str,
) -> crate::model::Workspace {
    manager
        .create_workspace(&repository.id, name.into(), None, None, None)
        .await
        .unwrap()
}

/// The restore of `execution` waiting for admission over its wrapper's link,
/// once the link counts.
pub(super) async fn await_restore(
    manager: &std::sync::Arc<Manager>,
    execution: &ExecutionId,
) -> tokio::task::JoinHandle<Result<Grant>> {
    let waiting = {
        let (manager, execution) = (manager.clone(), execution.to_owned());
        tokio::spawn(async move {
            let link = manager.agents.agent_queue().link_restore(&execution);
            manager.agents.await_restore_admission(&link).await
        })
    };
    bounded(async {
        while !manager
            .agents
            .agent_queue()
            .restore_links()
            .contains(execution)
        {
            tokio::task::yield_now().await;
        }
    })
    .await;
    waiting
}

pub(super) fn publish_recovery(manager: &Manager) {
    let epoch = manager.agents.overload_recovery().epoch();
    manager.agents.overload_recovery().publish(Some(epoch));
}

/// Each entry's name, kind and state in queue order, without the failed
/// launches, which wait for nothing.
pub(super) async fn queue(manager: &Manager) -> Vec<(String, QueueKind, QueueState)> {
    live(manager)
        .await
        .into_iter()
        .map(|entry| (entry.name.into(), entry.kind, entry.state))
        .collect()
}

/// The entries that wait or start, without the failed launches.
pub(super) async fn live(manager: &Manager) -> Vec<crate::model::QueueEntry> {
    let mut entries = manager.agents.queue_entries(manager).await.unwrap();
    entries.retain(|entry| entry.state != QueueState::Failed);
    entries
}

use QueueKind::Restore;
use QueueState::{Queued, Starting};
use shoal_ids::queue_entry_ids::QueueEntryId;

#[tokio::test]
async fn launches_in_an_existing_workspace_start_from_the_daemon() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    manager.publish_config(global_config(
        "[machine]\nmax_heavy_agents = 1000\n[machine.overload.disk]\nenabled = false",
    ));
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let child = manager
        .spaces
        .create_child(
            &*manager,
            &SpaceSelector::id(&parent.id),
            "child".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    // Without a limit the daemon still starts it rather than the caller.
    let admission = manager
        .agents
        .queue_add(&*manager, launch_in(&child, false))
        .await
        .unwrap();
    let entry = admission.entry.unwrap();
    assert!(!admission.start);
    assert_eq!(entry.opens.as_ref(), Some(&child.id));
    assert_eq!(entry.space_id, None);
    assert_eq!(entry.state, QueueState::Starting);
    assert!(started(&entry.id));
    // A launch that has yet to start keeps its workspace from cleanup.
    assert_eq!(
        manager
            .spaces
            .cleanup_reason(&*manager, &crate::model::Space::from(child.clone()))
            .await,
        crate::model::CleanupReason::QueuedAgent
    );
    // Its worker may attach only the workspace it opens.
    manager
        .agents
        .queue_attach(&*manager, &entry.id, &SpaceSelector::id(&parent.id))
        .await
        .unwrap_err();
    manager
        .agents
        .queue_attach(&*manager, &entry.id, &SpaceSelector::id(&child.id))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_scoped_caller_holds_reads_and_fails_only_the_launches_it_reaches() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 2);
    let parent = create(&manager, &repo, "parent").await;
    let caller = crate::daemon::scope::Caller {
        space_id: parent.id.clone(),
        execution: None,
        granted: None,
        access: Access::Full,
        vouched: None,
    };
    let child = manager
        .spaces
        .create_child(
            &*manager,
            &SpaceSelector::id(&parent.id),
            "own".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let own = manager
        .agents
        .queue_add(&*manager, launch_in(&child, false))
        .await
        .unwrap();
    let theirs = add(&manager, &repo, "theirs").await;
    manager
        .agents
        .executions()
        .issue_scope("scope".into(), caller)
        .await;
    for (entry, allowed) in [(own.entry.unwrap(), true), (theirs.entry.unwrap(), false)] {
        let methods = [
            crate::protocol::QueueMethod::QueueHold {
                entry: entry.id.clone(),
            },
            crate::protocol::QueueMethod::QueuedPlan {
                entry: entry.id.clone(),
            },
            crate::protocol::QueueMethod::QueueFail {
                entry: entry.id.clone(),
                error: "failed".into(),
            },
        ];
        for mut method in methods {
            let authorized =
                crate::daemon::scope::authorize(&manager, Some(&"scope".into()), &mut method).await;
            assert_eq!(authorized.is_ok(), allowed, "{}", entry.name);
        }
    }
}

#[tokio::test]
async fn workspace_processes_start_agents_only_below_their_workspace() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let create = |name: &str| manager.create_workspace(&repo.id, name.into(), None, None, None);
    let parent = create("parent").await.unwrap();
    let sibling = create("sibling").await.unwrap();
    let child = manager
        .spaces
        .create_child(
            &*manager,
            &SpaceSelector::id(&parent.id),
            "child".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    manager
        .agents
        .executions()
        .issue_scope(
            "scope".into(),
            crate::daemon::scope::Caller {
                space_id: parent.id.clone(),
                execution: None,
                granted: None,
                access: Access::Full,
                vouched: None,
            },
        )
        .await;
    // It queues the worker of a workspace below its own, and only there.
    let worker = |workspace: &shoal_ids::space_ids::SpaceId, now: bool| {
        crate::protocol::QueueMethod::QueueAgent {
            workspace: SpaceSelector::id(workspace),
            launch: Box::new(crate::protocol::operations::LaunchRequest {
                now,
                ..crate::protocol::operations::LaunchRequest::new(Some("claude".into()))
            }),
            blockers: Vec::new(),
            pr: None,
        }
    };
    for (workspace, allowed) in [(&child.id, true), (&parent.id, false), (&sibling.id, false)] {
        let mut method = worker(workspace, false);
        let authorized =
            crate::daemon::scope::authorize(&manager, Some(&"scope".into()), &mut method).await;
        assert_eq!(authorized.is_ok(), allowed, "{workspace:?}");
    }
    // Only the user starts one regardless of the limits.
    let mut method = worker(&child.id, true);
    let authorized =
        crate::daemon::scope::authorize(&manager, Some(&"scope".into()), &mut method).await;
    assert!(authorized.is_err());
}

#[tokio::test]
async fn a_workspace_reserved_for_removal_refuses_queued_launches() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let child = manager
        .create_workspace(&repo.id, "child".into(), None, None, None)
        .await
        .unwrap();
    // Removal reserves the workspace before its final checks; the queue
    // insertion then refuses the launch in the same transaction.
    manager
        .spaces
        .reserve_lifecycle(&child.id, crate::state::WorkspaceState::Removing)
        .await
        .unwrap();
    manager
        .agents
        .queue_add(&*manager, launch_in(&child, false))
        .await
        .unwrap_err();
    assert!(
        manager
            .agents
            .queue_entries(&*manager,)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn an_idle_stopped_agent_frees_its_slot_until_an_update_restores_it() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    let idle = create(&manager, &repo, "idle").await;
    let (execution, _stop) = connected_agent(&manager, &idle).await;
    manager
        .agents
        .set_agent_state(
            &*manager,
            &SpaceSelector::id(&idle.id),
            crate::state::AgentState::Idle,
            Some(execution.clone()),
            None,
        )
        .await
        .unwrap();
    assert!(
        manager
            .agents
            .stop_for_overload(&*manager, crate::daemon::overload::Pressure::Memory)
            .await
    );
    manager
        .agents
        .queue_agent_restore(&*manager, &execution)
        .await
        .unwrap();
    // Until its processes are proven stopped the agent keeps its slot.
    let held = add(&manager, &repo, "held").await;
    assert!(!admitted(&held));
    manager
        .agents
        .queue_remove(&*manager, &manager.queued("held").await, None)
        .await
        .unwrap();
    let restoring = await_restore(&manager, &execution).await;
    // Healthy readings do not restore it, and its slot is free for a launch.
    publish_recovery(&manager);
    let launch = add(&manager, &repo, "later").await;
    assert!(admitted(&launch));
    manager.agents.dispatch_queue(&*manager).await.unwrap();
    assert!(manager.agents.overload_recovery().published().is_some());
    let entries = manager.agents.queue_entries(&*manager).await.unwrap();
    let restore = entries.iter().find(|entry| entry.name == "idle").unwrap();
    assert_eq!(restore.state, Queued);
    assert!(restore.waits_for_update);
    let asleep = manager
        .spaces
        .workspace_by_id(&idle.id)
        .await
        .unwrap()
        .asleep;
    assert_eq!(
        asleep.map(|asleep| (asleep.reason, asleep.wakes_when)),
        Some((
            "critical memory pressure".into(),
            crate::daemon::agents::WAKES_ON_UPDATE.into()
        ))
    );

    // An update makes it due; it waits for the launch's slot like a launch.
    manager
        .coordination
        .send_space_message(
            &*manager,
            None,
            &SpaceSelector::id(&idle.id),
            "Review requested".into(),
            Vec::new(),
        )
        .await
        .unwrap();
    manager.agents.dispatch_queue(&*manager).await.unwrap();
    assert!(
        queue(&manager)
            .await
            .contains(&("idle".into(), Restore, Queued))
    );
    let launched = launch.entry.unwrap().id;
    manager
        .agents
        .fail_queue_entry(&*manager, &launched, "test")
        .await
        .unwrap();
    manager.agents.dispatch_queue(&*manager).await.unwrap();
    assert_eq!(bounded(restoring).await.unwrap().unwrap(), Grant::Update);
    assert_eq!(queue(&manager).await, [("idle".into(), Restore, Starting)]);
    // The update's admission holds through later stops.
    manager.agents.overload_recovery().reset();
    assert!(
        manager
            .agents
            .confirm_restore(&*manager, &execution, &idle.id, Grant::Update)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn a_message_to_every_agent_restores_an_idle_stopped_agent() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    let idle = create(&manager, &repo, "idle").await;
    let (execution, _stop) = connected_agent(&manager, &idle).await;
    manager
        .agents
        .set_agent_state(
            &*manager,
            &SpaceSelector::id(&idle.id),
            crate::state::AgentState::Idle,
            Some(execution.clone()),
            None,
        )
        .await
        .unwrap();
    assert!(
        manager
            .agents
            .stop_for_overload(&*manager, crate::daemon::overload::Pressure::Memory)
            .await
    );
    manager
        .agents
        .queue_agent_restore(&*manager, &execution)
        .await
        .unwrap();
    let restoring = await_restore(&manager, &execution).await;
    manager.agents.dispatch_queue(&*manager).await.unwrap();
    assert!(manager.agents.queue_entries(&*manager,).await.unwrap()[0].waits_for_update);
    let reached = manager
        .coordination
        .send_all_message(&*manager, None, None, "Rebase onto main".into(), Vec::new())
        .await
        .unwrap();
    assert_eq!(
        crate::daemon::test_support::names(&reached.reached.on_start.workspaces),
        ["idle"]
    );
    manager.agents.dispatch_queue(&*manager).await.unwrap();
    assert_eq!(bounded(restoring).await.unwrap().unwrap(), Grant::Update);
}

/// An agent restored after a load stop mid-turn reports its turns again:
/// the turn it ends leaves it idle, and activity on its PR then reaches the
/// wait that wakes it.
#[tokio::test]
async fn a_restored_agent_ends_its_turn_and_an_update_reaches_it() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    let loaded = create(&manager, &repo, "loaded").await;
    let space = SpaceSelector::id(&loaded.id);
    let (execution, _stop) = connected_agent(&manager, &loaded).await;
    let working = crate::state::AgentState::Working;
    let report = |state| {
        manager
            .agents
            .set_agent_state(&*manager, &space, state, Some(execution.clone()), None)
    };
    report(working).await.unwrap();
    assert!(
        manager
            .agents
            .stop_for_overload(&*manager, crate::daemon::overload::Pressure::Memory)
            .await
    );
    manager
        .agents
        .queue_agent_restore(&*manager, &execution)
        .await
        .unwrap();
    let link = manager.agents.agent_queue().link_restore(&execution);
    publish_recovery(&manager);
    manager.agents.dispatch_queue(&*manager).await.unwrap();
    let grant = bounded(manager.agents.await_restore_admission(&link))
        .await
        .unwrap();
    drop(link);
    let resumed = manager
        .agents
        .resume_agent_execution(&*manager, &execution, Some(grant));
    assert!(resumed.await.unwrap());

    report(working).await.unwrap();
    let turn_end = crate::protocol::TurnEnd::default();
    let ended =
        manager
            .agents
            .end_agent_turn(&*manager, &space, Some(execution.clone()), None, turn_end);
    assert_eq!(ended.await.unwrap().state, crate::state::AgentState::Idle);
    let id = loaded.id.clone();
    let current = manager
        .store
        .read(move |db| crate::daemon::turn_state::current(db, &id))
        .await
        .unwrap();
    assert_eq!(
        current.map(|status| status.state),
        Some(crate::state::AgentState::Idle)
    );

    crate::daemon::links::wait::test_support::comment_on_watched_pr(&manager, &loaded.id).await;
    let woken = bounded(manager.links.wait_space_updates(&*manager, &space))
        .await
        .unwrap();
    assert_eq!(woken.updates.len(), 1, "{woken:?}");
}

#[tokio::test]
async fn an_update_restores_an_idle_agent_only_once_disk_space_recovered() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    // Disk protection stays on for its readings, while admission reads
    // every filesystem as free.
    crate::daemon::capacity::fake_disk_free(u64::MAX);
    manager.publish_config(global_config("[machine]\nmax_heavy_agents = 1000"));
    let idle = create(&manager, &repo, "idle").await;
    let (execution, _stop) = connected_agent(&manager, &idle).await;
    manager
        .agents
        .set_agent_state(
            &*manager,
            &SpaceSelector::id(&idle.id),
            crate::state::AgentState::Idle,
            Some(execution.clone()),
            None,
        )
        .await
        .unwrap();
    assert!(
        manager
            .agents
            .stop_agents_for_disk_space(&*manager, &crate::model::Protection::load("low disk"))
            .await
            .len()
            == 1
    );
    manager
        .agents
        .queue_agent_restore(&*manager, &execution)
        .await
        .unwrap();
    let restoring = await_restore(&manager, &execution).await;
    manager
        .coordination
        .send_space_message(
            &*manager,
            None,
            &SpaceSelector::id(&idle.id),
            "Review requested".into(),
            Vec::new(),
        )
        .await
        .unwrap();
    // No reading has found the cleanup threshold free yet.
    manager.config_state.publish_disk_reading(None);
    manager.agents.dispatch_queue(&*manager).await.unwrap();
    assert_eq!(queue(&manager).await, [("idle".into(), Restore, Queued)]);
    let generation = manager.config_state.generation();
    manager.config_state.publish_disk_reading(Some(generation));
    manager.agents.dispatch_queue(&*manager).await.unwrap();
    assert_eq!(bounded(restoring).await.unwrap().unwrap(), Grant::Update);
}

#[tokio::test]
async fn a_oneshot_run_starts_beside_the_agents_without_a_slot() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    let workspace = create(&manager, &repo, "first").await;
    let first = manager
        .agents
        .queue_add(&*manager, launch_in(&workspace, false))
        .await;
    let first = first.unwrap().entry.unwrap();
    let launch = add(&manager, &repo, "launch").await.entry.unwrap();
    let mut review = (manager.agents.agent_queue()).request_oneshot(
        &repo.id,
        serving(&workspace.id),
        false,
        || {},
    );
    manager
        .agents
        .queue_attach(&*manager, &first.id, &SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    manager
        .agents
        .consume_queue_entry(&first.id, &workspace.id)
        .await;
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();

    // The run starts, and holds no slot, so the launch takes the one free
    // slot beside it.
    manager.agents.dispatch_queue(&*manager).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), review.admitted())
        .await
        .unwrap();
    assert!(started(&launch.id), "the run holds no slot");
}

/// What a one-shot run of `space`'s work serves, queued after everything.
pub(super) fn serving(
    space: &shoal_ids::space_ids::SpaceId,
) -> crate::daemon::agent_queue::Serving {
    crate::daemon::agent_queue::Serving {
        space: space.clone(),
        queued_after: i64::MAX,
    }
}

/// A failed `child` workspace in `repo` with a launch waiting there, and the
/// manager without an agent limit.
async fn launch_waiting_for_setup() -> (tempfile::TempDir, Arc<Manager>, Workspace, QueueEntry) {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    manager.publish_config(global_config(
        "[machine]\nmax_heavy_agents = 1000\n[machine.overload.disk]\nenabled = false",
    ));
    let child = manager
        .create_workspace(&repo.id, "child".into(), None, None, None)
        .await
        .unwrap();
    manager
        .spaces
        .set_state(
            &child.id,
            WorkspaceState::Failed,
            Some("setup failed".into()),
        )
        .await
        .unwrap();
    let launch = |now| launch_in(&child, now);
    // One that would start now needs the workspace ready.
    manager
        .agents
        .queue_add(&*manager, launch(true))
        .await
        .unwrap_err();
    let admission = manager
        .agents
        .queue_add(&*manager, launch(false))
        .await
        .unwrap();
    let entry = admission.entry.unwrap();
    assert!(!admission.start);
    assert_eq!(entry.state, QueueState::Queued);
    assert_eq!(entry.blockers, vec!["setup of child".to_owned()]);
    (temp, manager, child, entry)
}

/// Start a preparation of `workspace` that sets it up and runs its
/// post-setup hook, as a retried setup does, readying it.
async fn retry_setup(manager: &Manager, workspace: &Workspace) -> String {
    manager
        .spaces
        .set_state(&workspace.id, WorkspaceState::Ready, None)
        .await
        .unwrap();
    let spec = OperationSpec::Prepare {
        workspace: SpaceSelector::id(&workspace.id),
        steps: Preparation {
            setup: true,
            post_setup: true,
            ..Default::default()
        },
    };
    manager
        .spaces
        .start_operation(spec, None, None)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_launch_in_a_workspace_whose_setup_failed_waits_until_its_hook_ran() {
    let (_temp, manager, child, entry) = launch_waiting_for_setup().await;
    let operation = retry_setup(&manager, &child).await;
    let _worker = crate::daemon::operations::fake::take(&operation);
    // A ready workspace whose post-setup hook has yet to run holds it back.
    manager.agents.dispatch_queue(&*manager).await.unwrap();
    assert!(!started(&entry.id));

    let ended = Report::Step(Step::PostSetupEnded);
    crate::daemon::requests::operations::entries::report(&manager, &operation, ended, None)
        .await
        .unwrap();
    manager.agents.dispatch_queue(&*manager).await.unwrap();
    assert_eq!(queue(&manager).await[0].2, QueueState::Starting);
    assert!(started(&entry.id));
}

#[tokio::test]
async fn a_launch_keeps_waiting_when_its_workspaces_post_setup_hook_fails() {
    let (_temp, manager, child, entry) = launch_waiting_for_setup().await;
    let operation = retry_setup(&manager, &child).await;
    let worker = crate::daemon::operations::fake::take(&operation);
    let failed = Ended::Failed(Failure::HookFailed {
        message: "hook failed".into(),
    });
    let report = Report::Ended(failed);
    crate::daemon::requests::operations::entries::report(&manager, &operation, report, None)
        .await
        .unwrap();
    worker.exit(0);
    manager.agents.dispatch_queue(&*manager).await.unwrap();
    assert!(!started(&entry.id));
    let entries = manager.agents.queue_entries(&*manager).await.unwrap();
    assert_eq!(entries[0].blockers, vec!["setup of child".to_owned()]);
}

#[tokio::test]
async fn a_launch_left_waiting_for_setup_frees_the_slot_of_the_start_that_failed() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    // The start in `child` holds the only slot.
    let child = create(&manager, &repo, "child").await;
    let starting = manager
        .agents
        .queue_add(&*manager, launch_in(&child, false))
        .await;
    let starting = starting.unwrap().entry.unwrap();
    assert_eq!(starting.state, QueueState::Starting);
    manager
        .agents
        .queue_attach(&*manager, &starting.id, &SpaceSelector::id(&child.id))
        .await
        .unwrap();
    manager
        .spaces
        .set_state(
            &child.id,
            WorkspaceState::Failed,
            Some("setup failed".into()),
        )
        .await
        .unwrap();
    // Its agent, left waiting for the setup, takes its place.
    let waiting = manager
        .agents
        .queue_add(&*manager, launch_in(&child, false))
        .await
        .unwrap()
        .entry
        .unwrap();
    let entries = manager.agents.queue_entries(&*manager).await.unwrap();
    let ids: Vec<_> = entries.iter().map(|entry| &entry.id).collect();
    assert_eq!(ids, [&waiting.id]);
    assert!(admitted(&add(&manager, &repo, "other").await));
}

#[tokio::test]
async fn a_start_gives_its_queue_entry_the_workspace_it_reopens() {
    use crate::daemon::starts::{Opening, Preparing};
    use crate::protocol::operations::WorkspaceStart;
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    let child = create(&manager, &repo, "child").await;
    let starting = manager
        .agents
        .queue_add(&*manager, launch_in(&child, false))
        .await;
    let starting = starting.unwrap().entry.unwrap();
    // No caller attaches the entry: the start alone gives it the workspace.
    let opening = Opening {
        start: WorkspaceStart::Workspace(SpaceSelector::id(&child.id)),
        base: None,
        new: Default::default(),
        parent: None,
    };
    let preparing = Preparing {
        issue: None,
        pr: None,
        agent: None,
        waiting: None,
        queue_entry: Some(starting.id.clone()),
        blockers: Vec::new(),
        unfollowed: false,
        env: None,
    };
    let started = manager
        .spaces
        .start_workspace(
            &manager.daemon(),
            (&repo.id).into(),
            opening,
            preparing,
            None,
        )
        .await
        .unwrap();
    let _worker = (started.operation.as_deref()).map(crate::daemon::operations::fake::take);
    let entries = manager.agents.queue_entries(&*manager).await.unwrap();
    assert_eq!(entries.len(), 1, "{entries:?}");
    assert_eq!(entries[0].space_id.as_ref(), Some(&child.id));
}

#[tokio::test]
async fn a_launch_waits_for_a_setup_the_daemon_never_started() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    manager.publish_config(global_config(
        "[machine]\nmax_heavy_agents = 1000\n[machine.overload.disk]\nenabled = false",
    ));
    let child = manager
        .create_workspace(&repo.id, "child".into(), None, None, None)
        .await
        .unwrap();
    // Its setup was refused before it registered, so it stays preparing
    // with nothing running.
    manager
        .spaces
        .set_state(&child.id, WorkspaceState::Preparing, None)
        .await
        .unwrap();
    let entry = manager
        .agents
        .queue_add(&*manager, launch_in(&child, false))
        .await
        .unwrap()
        .entry
        .unwrap();
    assert_eq!(entry.state, QueueState::Queued);
    assert_eq!(entry.blockers, vec!["setup of child".to_owned()]);
}

mod actions;
mod blockers;
mod chats;
mod failures;
mod openings;
mod setup_waits;
mod stopped;
