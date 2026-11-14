use super::*;
use crate::client::assignments::Done;
use crate::client::execution_ids::ExecutionId;
use crate::client::priority::Priority;
use crate::client::repository_ids::RepositoryId;
use crate::client::space_names::SpaceSelector;
use crate::daemon::workspace::agents::ProtectionExt;
use crate::execution::recovery::RecordExt;
use crate::{
    daemon::{
        notifications::NotificationKind,
        test_support::{manager, repository},
        workspace::{ExecutionKind, StartedExecution},
        workspace_files::Attacher,
    },
    execution::recovery::Record,
    model::Workspace,
    state::{AgentState, Sleep},
};

/// How a test workspace keeps its agent.
#[derive(Clone, Copy)]
struct Setup {
    state: AgentState,
    done: bool,
    held: bool,
    recover: bool,
    /// The mode its agent runs in.
    mode: &'static str,
}

const HELD_DONE_IDLE: Setup = Setup {
    state: AgentState::Idle,
    done: true,
    held: true,
    recover: true,
    mode: "worker",
};

/// A workspace named `name` whose agent `setup` describes.
async fn agent(
    manager: &Manager,
    repository_id: &RepositoryId,
    name: &str,
    setup: Setup,
) -> (Workspace, StartedExecution) {
    let workspace = manager
        .create_workspace(repository_id, name.into(), None, None, None)
        .await
        .unwrap();
    let execution = manager
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let agent = crate::execution::TrackedAgent {
        mode: setup.mode.into(),
        ..crate::execution::TrackedAgent::named("claude")
    };
    manager
        .track_agent(
            &execution.plan.id,
            &agent,
            &workspace.id,
            &workspace.name,
            setup.recover,
            std::time::Duration::ZERO,
        )
        .await;
    manager
        .set_agent_state(
            &SpaceSelector::id(&workspace.id),
            setup.state,
            Some(execution.plan.id.clone()),
            None,
        )
        .await
        .unwrap();
    if setup.held {
        manager
            .acquire_hold(
                &SpaceSelector::id(&workspace.id),
                "app".into(),
                None,
                crate::model::HoldPlacer::User,
            )
            .await
            .unwrap();
    }
    if setup.done {
        manager
            .mark_done(
                &SpaceSelector::id(&workspace.id),
                Done {
                    cleanup: true,
                    ..Done::default()
                },
                &Attacher::User,
            )
            .await
            .unwrap();
    }
    (workspace, execution)
}

/// Set when the workspace's prompt cache expires, `offset` seconds from now.
async fn cache_expires_in(manager: &Manager, workspace: &Workspace, offset: i64) {
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE space_agent_state
                 SET prompt_cache_expires_at=unixepoch()+?2 WHERE space_id=?1",
                rusqlite::params![id, offset],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn a_held_done_workspaces_idle_agent_stops_once_its_cache_expires() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (workspace, execution) = agent(&manager, &repo.id, "held", HELD_DONE_IDLE).await;
    cache_expires_in(&manager, &workspace, 600).await;
    manager.stop_idle_agents().await.unwrap();
    assert!(!*execution.stop.borrow(), "a warm cache keeps the agent");

    cache_expires_in(&manager, &workspace, 0).await;
    manager.stop_idle_agents().await.unwrap();
    assert!(*execution.stop.borrow());
    let path =
        crate::execution::recovery::record_path(&manager.paths, &workspace.id, &execution.plan.id);
    let record = Record::load(&path).unwrap().unwrap();
    assert_eq!(record.turn, Some(AgentState::Idle));
    let notifications = manager.notifications(false, 10).await.unwrap();
    let stopped: Vec<_> = notifications
        .iter()
        .filter(|event| event.kind == NotificationKind::AgentStopped)
        .collect();
    assert_eq!(stopped.len(), 1);
}

#[tokio::test]
async fn other_agents_keep_running() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let mut executions = Vec::new();
    for (name, setup) in [
        (
            "working",
            Setup {
                state: AgentState::Working,
                ..HELD_DONE_IDLE
            },
        ),
        (
            "unheld",
            Setup {
                held: false,
                ..HELD_DONE_IDLE
            },
        ),
        (
            "unfinished",
            Setup {
                done: false,
                ..HELD_DONE_IDLE
            },
        ),
        (
            "unrestorable",
            Setup {
                recover: false,
                ..HELD_DONE_IDLE
            },
        ),
    ] {
        executions.push((name, agent(&manager, &repo.id, name, setup).await.1));
    }
    manager.stop_idle_agents().await.unwrap();
    for (name, execution) in executions {
        assert!(!*execution.stop.borrow(), "{name} keeps running");
    }
}

#[tokio::test]
async fn sleep_idle_stops_any_agent_once_its_cache_expires() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let unfinished = Setup {
        done: false,
        held: false,
        ..HELD_DONE_IDLE
    };
    let (workspace, execution) = agent(&manager, &repo.id, "idle", unfinished).await;
    manager.stop_idle_agents().await.unwrap();
    assert!(!*execution.stop.borrow(), "[sleep] idle is opt-in");

    manager
        .write_local_config(&repo.id, Some("[sleep]\nidle = true\n".into()))
        .await
        .unwrap();
    cache_expires_in(&manager, &workspace, 60).await;
    manager.stop_idle_agents().await.unwrap();
    assert!(!*execution.stop.borrow(), "a warm cache keeps the agent");

    cache_expires_in(&manager, &workspace, 0).await;
    manager.stop_idle_agents().await.unwrap();
    assert!(*execution.stop.borrow());
    let notifications = manager.notifications(false, 10).await.unwrap();
    assert!(
        notifications
            .iter()
            .any(|event| event.kind == NotificationKind::AgentStopped),
        "{notifications:?}"
    );
}

#[tokio::test]
async fn a_cache_renewed_after_the_agent_was_chosen_keeps_it() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (workspace, execution) = agent(&manager, &repo.id, "held", HELD_DONE_IDLE).await;
    let chosen = chosen_held(&manager, &execution.plan.id).await;
    // A turn ended after the sweep chose the agent.
    cache_expires_in(&manager, &workspace, 600).await;
    let turns = manager.agent_turns().await.unwrap();
    manager.stop_idle(turns, chosen).await;
    assert!(!*execution.stop.borrow());
}

#[tokio::test]
async fn a_turn_that_starts_between_the_choice_and_the_stop_keeps_the_agent() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (workspace, execution) = agent(&manager, &repo.id, "held", HELD_DONE_IDLE).await;
    let id = execution.plan.id.clone();
    let chosen = chosen_held(&manager, &id).await;
    let turns = manager.agent_turns().await.unwrap();
    // A whole turn passes after the turns were read, so they show the agent
    // idle as it was when it was chosen.
    for state in [AgentState::Working, AgentState::Idle] {
        manager
            .set_agent_state(
                &SpaceSelector::id(&workspace.id),
                state,
                Some(id.clone()),
                None,
            )
            .await
            .unwrap();
    }
    cache_expires_in(&manager, &workspace, 0).await;
    manager.stop_idle(turns, chosen).await;
    assert!(!*execution.stop.borrow(), "the agent keeps running");
    assert!(manager.agent_overload(&id).await.is_none());
}

#[tokio::test]
async fn a_wake_after_an_idle_stop_leaves_its_update_for_the_restore() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (workspace, execution) = agent(&manager, &repo.id, "held", HELD_DONE_IDLE).await;
    let id = execution.plan.id.clone();
    manager.stop_idle_agents().await.unwrap();
    assert!(*execution.stop.borrow());
    let woken = manager
        .start_woken_turn(&SpaceSelector::id(&workspace.id), Some(id))
        .await
        .unwrap();
    assert!(!woken, "the stop came first");
}

#[tokio::test]
async fn a_wake_after_a_recorded_stop_records_no_turn() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (workspace, execution) = agent(&manager, &repo.id, "held", HELD_DONE_IDLE).await;
    let id = execution.plan.id.clone();
    let before = manager.reported_turns().await.unwrap()[&id];
    // The stop is recorded but has not reached the wrapper yet.
    let protection = Protection::idle_held();
    assert!(
        manager
            .record_agent_stop(&id, &protection, None)
            .await
            .unwrap()
    );
    let woken = manager
        .start_woken_turn(&SpaceSelector::id(&workspace.id), Some(id.clone()))
        .await
        .unwrap();
    assert!(!woken, "the update waits for the restore");
    assert_eq!(manager.reported_turns().await.unwrap()[&id], before);
}

#[tokio::test]
async fn an_idle_stop_after_a_wake_keeps_the_agent() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (workspace, execution) = agent(&manager, &repo.id, "held", HELD_DONE_IDLE).await;
    let id = execution.plan.id.clone();
    let chosen = chosen_held(&manager, &id).await;
    let turns = manager.agent_turns().await.unwrap();
    let woken = manager
        .start_woken_turn(&SpaceSelector::id(&workspace.id), Some(id))
        .await
        .unwrap();
    assert!(woken);
    manager.stop_idle(turns, chosen).await;
    assert!(!*execution.stop.borrow());
}

/// The sweep's choice to stop `execution` as idle in a held done workspace,
/// in the turn it reports now.
async fn chosen_held(manager: &Manager, execution: &ExecutionId) -> HashMap<ExecutionId, Chosen> {
    let stop = Chosen {
        protection: Protection::idle_held(),
        turn: manager.reported_turns().await.unwrap()[execution],
        cache_expired_at: Some(crate::time::UnixTime::now()),
    };
    HashMap::from([(execution.to_owned(), stop)])
}

#[tokio::test]
async fn a_blocked_agent_stops_once_its_turn_ends_and_waits_for_the_space_to_clear() {
    use crate::client::progress::ProgressChange;
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let working = Setup {
        state: AgentState::Working,
        done: false,
        held: false,
        ..HELD_DONE_IDLE
    };
    let (workspace, execution) = agent(&manager, &repo.id, "blocked", working).await;
    let selector = SpaceSelector::id(&workspace.id);
    let blocker = crate::model::Blocker::Pr("https://github.com/team/app/pull/8".parse().unwrap());
    let change = ProgressChange::AddBlocker {
        blocker: blocker.clone(),
        step: None,
        reason: "Needs the API".into(),
    };
    manager.set_progress(&selector, change).await.unwrap();
    cache_expires_in(&manager, &workspace, 600).await;
    manager.stop_idle_agents().await.unwrap();
    assert!(
        !*execution.stop.borrow(),
        "a working agent finishes its turn"
    );

    manager
        .set_agent_state(
            &selector,
            AgentState::Idle,
            Some(execution.plan.id.clone()),
            None,
        )
        .await
        .unwrap();
    cache_expires_in(&manager, &workspace, 600).await;
    manager.stop_idle_agents().await.unwrap();
    assert!(
        *execution.stop.borrow(),
        "a warm cache keeps no blocked agent"
    );
    assert!(manager.restore_waits_for_unblock(&workspace.id, &execution.plan.id));
    // A message waits for its restore, which starts its turn however the
    // space clears.
    let waiting = manager.agent_messages(&selector).await.unwrap();
    assert!(
        waiting
            .iter()
            .any(|message| message.message == crate::daemon::blockers::STOPPED_BLOCKED)
    );
    // Its stop notifies no one: the space's blocking does.
    let notifications = manager.notifications(false, 10).await.unwrap();
    assert!(
        !notifications
            .iter()
            .any(|event| event.kind == NotificationKind::AgentStopped)
    );
    // Its waiting restore keeps the workspace from cleanup, unlike an idle
    // agent's, even with no update left waiting for it.
    let ids = waiting.iter().map(|message| message.id).collect();
    manager
        .mark_agent_messages_delivered(&selector, ids)
        .await
        .unwrap();
    let agent = crate::agent::Agent::from("claude");
    manager
        .queue_restore(&execution.plan.id, &agent, &workspace.id)
        .await
        .unwrap();
    let waiter = manager.agent_queue.wait_for_restore(&execution.plan.id);
    assert!(
        manager
            .idle_restores(&workspace.id)
            .await
            .unwrap()
            .is_empty()
    );
    drop(waiter);
    // An update does not make its restore due; the space clearing does.
    manager.publish_config(crate::daemon::test_support::global_config(
        "[machine.overload.disk]\nenabled = false",
    ));
    manager
        .queue_agent_message(&workspace.id, "PR activity".into())
        .await
        .unwrap();
    assert!(
        !manager
            .restore_due(&workspace.id, &execution.plan.id)
            .await
            .unwrap()
    );
    // `shoal restore` restores it now, telling it what still blocks it.
    let entries = manager.queue_entries().await.unwrap();
    assert_eq!(entries[0].blockers, ["https://github.com/team/app/pull/8"]);
    manager
        .queue_stopped_restore(&selector, &execution.plan.id, Default::default())
        .await
        .unwrap();
    let messages = manager.agent_messages(&selector).await.unwrap();
    assert!(
        messages
            .iter()
            .any(|message| message.message.starts_with("Your work is still blocked:")),
        "{messages:?}"
    );
    let remove = ProgressChange::RemoveBlocker {
        blocker,
        step: None,
    };
    manager.set_progress(&selector, remove).await.unwrap();
    assert!(
        manager
            .restore_due(&workspace.id, &execution.plan.id)
            .await
            .unwrap()
    );
}

/// An idle chat whose agent runs in `mode`, in a repository under `root`
/// with `[sleep] idle` enabled and a heavy chat mode `pair`, whose prompt
/// cache has expired, and its agent's execution.
async fn idle_chat(
    manager: &std::sync::Arc<Manager>,
    root: &std::path::Path,
    mode: &str,
) -> (
    crate::model::Chat,
    crate::daemon::workspace::StartedExecution,
) {
    let repo = repository(root, "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager
        .write_local_config(
            &repo.id,
            Some("[sleep]\nidle = true\n[modes.pair]\nkind = 'chat'\n".into()),
        )
        .await
        .unwrap();
    let chat = manager
        .start_chat(repo.id.into(), "talk".into(), "claude".into(), None)
        .await
        .unwrap();
    // Stopped, so the execution registered below is its agent.
    manager.end_executions(&chat.id).await;
    let execution = manager
        .begin_agent_execution(chat.id.as_str())
        .await
        .unwrap();
    let agent = crate::execution::TrackedAgent {
        mode: mode.into(),
        ..crate::execution::TrackedAgent::named("claude")
    };
    manager
        .track_agent(
            &execution.plan.id,
            &agent,
            &chat.id,
            &chat.name,
            true,
            std::time::Duration::ZERO,
        )
        .await;
    manager
        .set_agent_state(
            &SpaceSelector::id(&chat.id),
            AgentState::Idle,
            Some(execution.plan.id.clone()),
            None,
        )
        .await
        .unwrap();
    let id = chat.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE space_agent_state SET prompt_cache_expires_at=unixepoch() WHERE space_id=?1",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    (chat, execution)
}

/// `[sleep] idle` stops a heavy chat's idle agent once its prompt cache
/// expires, as it stops a workspace's.
#[tokio::test]
async fn a_heavy_chats_idle_agent_stops_once_its_cache_expires() {
    let (root, manager) = manager().await;
    let (_, execution) = idle_chat(&manager, root.path(), "pair").await;

    manager.stop_idle_agents().await.unwrap();

    assert!(*execution.stop.borrow());
}

/// A light chat's idle agent keeps running with its prompt cache expired.
#[tokio::test]
async fn a_light_chats_idle_agent_keeps_running_once_its_cache_expires() {
    let (root, manager) = manager().await;
    let (_, execution) = idle_chat(&manager, root.path(), "chat").await;

    manager.stop_idle_agents().await.unwrap();

    assert!(!*execution.stop.borrow());
}

/// A blocked chat's idle agent stops as a workspace's does, its wrapper
/// waiting to restore it in place once the space clears.
#[tokio::test]
async fn a_blocked_chats_idle_agent_stops_until_its_space_clears() {
    use crate::client::progress::ProgressChange;
    let (root, manager) = manager().await;
    let (chat, execution) = idle_chat(&manager, root.path(), "chat").await;
    let blocker = crate::model::Blocker::Pr("https://github.com/team/app/pull/8".parse().unwrap());
    let change = ProgressChange::AddBlocker {
        blocker,
        step: None,
        reason: "Needs the API".into(),
    };
    (manager.set_progress(&SpaceSelector::id(&chat.id), change))
        .await
        .unwrap();

    manager.stop_idle_agents().await.unwrap();

    assert!(*execution.stop.borrow());
    assert!(manager.restore_waits_for_unblock(&chat.id, &execution.plan.id));
}

/// An agent whose work is not done, in a workspace nothing holds.
const UNFINISHED: Setup = Setup {
    done: false,
    held: false,
    ..HELD_DONE_IDLE
};

/// Why Shoal stopped the agent of `execution`, while it waits to restore.
async fn stop_reason(manager: &Manager, execution: &StartedExecution) -> Option<String> {
    let protection = manager.agent_overload(&execution.plan.id).await;
    protection.map(|protection| protection.reason)
}

#[tokio::test]
async fn a_sleeping_agent_sleeps_once_its_cache_expires() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (workspace, execution) = agent(&manager, &repo.id, "sleeps", UNFINISHED).await;
    let selector = SpaceSelector::id(&workspace.id);
    (manager.sleep(&selector, Some(Sleep::Turn))).await.unwrap();
    cache_expires_in(&manager, &workspace, 600).await;
    manager.stop_idle_agents().await.unwrap();
    assert!(!*execution.stop.borrow(), "a warm cache keeps the agent");

    cache_expires_in(&manager, &workspace, 0).await;
    manager.stop_idle_agents().await.unwrap();
    assert!(*execution.stop.borrow());
    assert_eq!(
        stop_reason(&manager, &execution).await,
        Some(Protection::slept().reason)
    );
}

/// A light workspace agent that sleeps only stops its idle wakes: neither
/// its sleep nor an expired cache under `[sleep] idle` stops it.
#[tokio::test]
async fn a_light_agent_that_sleeps_keeps_running() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let config = "[sleep]\nidle = true\n[modes.triage]\nweight = 'light'\n";
    (manager.write_local_config(&repo.id, Some(config.into())))
        .await
        .unwrap();
    let light = Setup {
        mode: "triage",
        ..UNFINISHED
    };
    let (workspace, execution) = agent(&manager, &repo.id, "sleeps", light).await;
    let selector = SpaceSelector::id(&workspace.id);
    (manager.sleep(&selector, Some(Sleep::Turn))).await.unwrap();
    cache_expires_in(&manager, &workspace, 0).await;

    manager.stop_idle_agents().await.unwrap();

    assert!(!*execution.stop.borrow());
}

#[tokio::test]
async fn under_the_speed_schedule_a_sleeping_agent_sleeps_at_once() {
    let (root, manager) = manager().await;
    manager.publish_config(crate::test_support::global_config(
        "[machine]\nschedule = \"speed\"\n",
    ));
    let repo = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (workspace, execution) = agent(&manager, &repo.id, "sleeps", UNFINISHED).await;
    let selector = SpaceSelector::id(&workspace.id);
    (manager.sleep(&selector, Some(Sleep::Permanent)))
        .await
        .unwrap();
    cache_expires_in(&manager, &workspace, 600).await;
    manager.stop_idle_agents().await.unwrap();
    assert!(*execution.stop.borrow());
}

/// Ask a question in `workspace`, which wakes its agent once answered.
async fn ask(manager: &Manager, workspace: &Workspace) {
    let ask = crate::client::questions::Ask {
        question: "Which database?".into(),
        recommendation: "SQLite".into(),
        reason: None,
        choices: Vec::new(),
    };
    manager
        .ask_question(
            &SpaceSelector::id(&workspace.id),
            ask,
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn an_agent_something_will_wake_sleeps_to_make_room_for_queued_work() {
    let (root, manager) = manager().await;
    manager.publish_config(crate::test_support::global_config(
        "[machine]\nmax_agents = 3\n[machine.overload.disk]\nenabled = false",
    ));
    let repo = repository(root.path(), "repo");
    let repo = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let mut agents = Vec::new();
    for name in ["asks", "urgent", "idle"] {
        let (workspace, execution) = agent(&manager, &repo.id, name, UNFINISHED).await;
        cache_expires_in(&manager, &workspace, 0).await;
        agents.push((workspace, execution));
    }
    let [(asks, asking), (urgent, ranked_higher), (_, nothing_wakes)] = &agents[..] else {
        unreachable!()
    };
    ask(&manager, asks).await;
    ask(&manager, urgent).await;
    manager
        .set_priority(None, &SpaceSelector::id(&urgent.id), Priority::URGENT)
        .await
        .unwrap();
    // Without queued work, nothing sleeps for room.
    manager.stop_idle_agents().await.unwrap();
    assert!(!*asking.stop.borrow());

    let launch = manager
        .queue_add(crate::daemon::workspace::agent_queue::tests::request(
            &repo, "later", false,
        ))
        .await
        .unwrap();
    assert!(!launch.start);
    manager.dispatch_queue().await.unwrap();
    manager.stop_idle_agents().await.unwrap();
    assert_eq!(
        stop_reason(&manager, asking).await,
        Some(Protection::making_room().reason)
    );
    assert!(!*ranked_higher.stop.borrow(), "a higher priority keeps it");
    assert!(!*nothing_wakes.stop.borrow(), "nothing would wake it");
}
