use super::*;
use crate::daemon::agents::ProtectionExt;
use crate::execution::recovery::RecordExt;
use crate::{
    daemon::{
        executions::{ExecutionKind, StartedExecution},
        notifications::AlertKind,
        test_support::{manager, repository},
    },
    execution::recovery::Record,
    model::Workspace,
    state::{AgentState, Sleep},
};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::space_names::SpaceSelector;
use shoal_schedule::priority::Priority;
use std::time::Duration;

/// How a test workspace keeps its agent.
#[derive(Clone, Copy)]
struct Setup {
    state: AgentState,
    recover: bool,
    /// The mode its agent runs in.
    mode: &'static str,
}

const IDLE: Setup = Setup {
    state: AgentState::Idle,
    recover: true,
    mode: "worker",
};

/// A time once the wait of an agent the user is not engaged with has
/// passed, whose wait is seconds.
fn later() -> UnixTime {
    UnixTime::now() + Duration::from_secs(60)
}

/// When the turn of the agent `execution` runs ended, as its idle report
/// recorded it.
async fn turn_end(manager: &Manager, execution: &StartedExecution) -> UnixTime {
    manager.reported_turns().await.unwrap()[&execution.plan.id].since
}

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
    let agent = shoal_protocol::executions::TrackedAgent {
        mode: setup.mode.into(),
        ..shoal_protocol::executions::TrackedAgent::named("claude")
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
async fn an_idle_agent_the_user_is_away_from_sleeps_seconds_after_its_turn() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (workspace, execution) = agent(&manager, &repo.id, "idle", IDLE).await;
    let ended = turn_end(&manager, &execution).await;
    manager.stop_idle_agents(ended).await.unwrap();
    assert!(!*execution.stop.borrow(), "its turn just ended");

    manager.stop_idle_agents(later()).await.unwrap();
    assert!(*execution.stop.borrow());
    assert_eq!(
        stop_reason(&manager, &execution).await,
        Some(Protection::away().reason)
    );
    let path =
        crate::execution::recovery::record_path(&manager.paths, &workspace.id, &execution.plan.id);
    let record = Record::load(&path).unwrap().unwrap();
    assert_eq!(record.turn, Some(AgentState::Idle));
    let notifications = manager.notifications.list(false, 10).await.unwrap();
    let stopped: Vec<_> = notifications
        .iter()
        .filter(|event| event.kind == AlertKind::AgentStopped)
        .collect();
    assert_eq!(stopped.len(), 1);
}

#[tokio::test]
async fn other_agents_keep_running() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let mut executions = Vec::new();
    for (name, setup) in [
        (
            "working",
            Setup {
                state: AgentState::Working,
                ..IDLE
            },
        ),
        (
            "unrestorable",
            Setup {
                recover: false,
                ..IDLE
            },
        ),
    ] {
        executions.push((name, agent(&manager, &repo.id, name, setup).await.1));
    }
    manager.stop_idle_agents(later()).await.unwrap();
    for (name, execution) in executions {
        assert!(!*execution.stop.borrow(), "{name} keeps running");
    }
}

#[tokio::test]
async fn a_turn_that_starts_between_the_choice_and_the_stop_keeps_the_agent() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (workspace, execution) = agent(&manager, &repo.id, "idle", IDLE).await;
    let id = execution.plan.id.clone();
    let chosen = chosen_away(&manager, &id).await;
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
    manager.stop_idle(turns, chosen).await;
    assert!(!*execution.stop.borrow(), "the agent keeps running");
    assert!(manager.agent_overload(&id).await.is_none());
}

#[tokio::test]
async fn an_interaction_between_the_choice_and_the_stop_keeps_the_agent() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (workspace, execution) = agent(&manager, &repo.id, "idle", IDLE).await;
    let id = execution.plan.id.clone();
    let chosen = chosen_away(&manager, &id).await;
    let turns = manager.agent_turns().await.unwrap();
    (manager.record_interaction(&workspace.id, UnixTime::now()))
        .await
        .unwrap();
    manager.stop_idle(turns, chosen).await;
    assert!(!*execution.stop.borrow(), "the user came back");
    assert!(manager.agent_overload(&id).await.is_none());
}

/// Report `change` to the subagents of the agent `execution` runs in
/// `workspace`.
async fn subagent(
    manager: &Manager,
    workspace: &Workspace,
    execution: &ExecutionId,
    change: crate::protocol::SubagentChange,
) {
    let space = SpaceSelector::id(&workspace.id);
    let now = crate::time::UnixTime::now();
    (manager.report_subagent(&space, Some(execution.clone()), change, now))
        .await
        .unwrap();
}

#[tokio::test]
async fn an_idle_agent_with_a_running_subagent_stops_once_the_subagent_stops() {
    use crate::protocol::SubagentChange;
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (workspace, execution) = agent(&manager, &repo.id, "held", IDLE).await;
    let id = execution.plan.id.clone();
    let agent_id = "a1".to_owned();
    let started = SubagentChange::Started {
        agent_id: agent_id.clone(),
    };
    subagent(&manager, &workspace, &id, started).await;
    manager.stop_idle_agents(later()).await.unwrap();
    assert!(!*execution.stop.borrow(), "its subagent keeps the agent");

    subagent(
        &manager,
        &workspace,
        &id,
        SubagentChange::Stopped { agent_id },
    )
    .await;
    manager.stop_idle_agents(later()).await.unwrap();
    assert!(*execution.stop.borrow());
}

#[tokio::test]
async fn a_subagent_that_starts_between_the_choice_and_the_stop_keeps_the_agent() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (workspace, execution) = agent(&manager, &repo.id, "held", IDLE).await;
    let id = execution.plan.id.clone();
    let chosen = chosen_away(&manager, &id).await;
    let turns = manager.agent_turns().await.unwrap();
    let started = crate::protocol::SubagentChange::Started {
        agent_id: "a1".into(),
    };
    subagent(&manager, &workspace, &id, started).await;
    manager.stop_idle(turns, chosen).await;
    assert!(!*execution.stop.borrow(), "the agent keeps running");
}

#[tokio::test]
async fn a_wake_after_an_idle_stop_leaves_its_update_for_the_restore() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (workspace, execution) = agent(&manager, &repo.id, "held", IDLE).await;
    let id = execution.plan.id.clone();
    manager.stop_idle_agents(later()).await.unwrap();
    assert!(*execution.stop.borrow());
    // An update restores it, not recovery.
    let path = crate::execution::recovery::record_path(&manager.paths, &workspace.id, &id);
    assert!(!Record::load(&path).unwrap().unwrap().for_load);
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
        .repositories
        .register(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (workspace, execution) = agent(&manager, &repo.id, "held", IDLE).await;
    let id = execution.plan.id.clone();
    let before = manager.reported_turns().await.unwrap()[&id];
    // The stop is recorded but has not reached the wrapper yet.
    let protection = Protection::away();
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
        .repositories
        .register(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (workspace, execution) = agent(&manager, &repo.id, "held", IDLE).await;
    let id = execution.plan.id.clone();
    let chosen = chosen_away(&manager, &id).await;
    let turns = manager.agent_turns().await.unwrap();
    let woken = manager
        .start_woken_turn(&SpaceSelector::id(&workspace.id), Some(id))
        .await
        .unwrap();
    assert!(woken);
    manager.stop_idle(turns, chosen).await;
    assert!(!*execution.stop.borrow());
}

/// The sweep's choice to stop `execution` as idle with the user away, in
/// the turn it reports now.
async fn chosen_away(manager: &Manager, execution: &ExecutionId) -> HashMap<ExecutionId, Chosen> {
    let stop = Chosen {
        protection: Protection::away(),
        turn: manager.reported_turns().await.unwrap()[execution],
        interacted_at: None,
    };
    HashMap::from([(execution.to_owned(), stop)])
}

#[tokio::test]
async fn a_blocked_agent_stops_once_its_turn_ends_and_waits_for_the_space_to_clear() {
    use shoal_protocol::progress::ProgressChange;
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let working = Setup {
        state: AgentState::Working,
        ..IDLE
    };
    let (workspace, execution) = agent(&manager, &repo.id, "blocked", working).await;
    let selector = SpaceSelector::id(&workspace.id);
    let blocker = shoal_protocol::progress::Blocker::Pr(
        "https://github.com/team/app/pull/8".parse().unwrap(),
    );
    let change = ProgressChange::AddBlocker {
        blocker: blocker.clone(),
        step: None,
        reason: "Needs the API".into(),
    };
    manager.set_progress(&selector, change).await.unwrap();
    manager.stop_idle_agents(UnixTime::now()).await.unwrap();
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
    manager.stop_idle_agents(UnixTime::now()).await.unwrap();
    assert!(
        *execution.stop.borrow(),
        "a blocked agent sleeps as its turn ends"
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
    let notifications = manager.notifications.list(false, 10).await.unwrap();
    assert!(
        !notifications
            .iter()
            .any(|event| event.kind == AlertKind::AgentStopped)
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
    let link = manager.agent_queue.link_restore(&execution.plan.id);
    assert!(
        manager
            .idle_restores(&workspace.id)
            .await
            .unwrap()
            .is_empty()
    );
    drop(link);
    // An update does not make its restore due; the space clearing does.
    manager.publish_config(crate::daemon::test_support::global_config(
        "[machine.overload.disk]\nenabled = false",
    ));
    manager
        .queue_agent_message(
            &workspace.id,
            crate::daemon::alerts::AlertKind::Message,
            "PR activity".into(),
        )
        .await
        .unwrap();
    assert!(
        !manager
            .restore_due(&workspace.id, &execution.plan.id)
            .await
            .unwrap()
    );
    // `shoal resume` restores it now, telling it what still blocks it.
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
/// with a heavy chat mode `pair`, and its agent's execution.
async fn idle_chat(
    manager: &std::sync::Arc<Manager>,
    root: &std::path::Path,
    mode: &str,
) -> (
    crate::model::Chat,
    crate::daemon::executions::StartedExecution,
) {
    let repo = repository(root, "repo");
    let repo = manager
        .repositories
        .register(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager
        .write_local_config(&repo.id, Some("[modes.pair]\nkind = 'chat'\n".into()))
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
    let agent = shoal_protocol::executions::TrackedAgent {
        mode: mode.into(),
        ..shoal_protocol::executions::TrackedAgent::named("claude")
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
    (chat, execution)
}

/// A heavy chat's idle agent sleeps as a workspace's does.
#[tokio::test]
async fn a_heavy_chats_idle_agent_sleeps() {
    let (root, manager) = manager().await;
    let (_, execution) = idle_chat(&manager, root.path(), "pair").await;

    manager.stop_idle_agents(later()).await.unwrap();

    assert!(*execution.stop.borrow());
}

/// A light chat's idle agent keeps running.
#[tokio::test]
async fn a_light_chats_idle_agent_keeps_running() {
    let (root, manager) = manager().await;
    let (_, execution) = idle_chat(&manager, root.path(), "chat").await;

    manager.stop_idle_agents(later()).await.unwrap();

    assert!(!*execution.stop.borrow());
}

/// A blocked chat's idle agent stops as a workspace's does, its wrapper
/// waiting to restore it in place once the space clears.
#[tokio::test]
async fn a_blocked_chats_idle_agent_stops_until_its_space_clears() {
    use shoal_protocol::progress::ProgressChange;
    let (root, manager) = manager().await;
    let (chat, execution) = idle_chat(&manager, root.path(), "chat").await;
    let blocker = shoal_protocol::progress::Blocker::Pr(
        "https://github.com/team/app/pull/8".parse().unwrap(),
    );
    let change = ProgressChange::AddBlocker {
        blocker,
        step: None,
        reason: "Needs the API".into(),
    };
    (manager.set_progress(&SpaceSelector::id(&chat.id), change))
        .await
        .unwrap();

    manager.stop_idle_agents(UnixTime::now()).await.unwrap();

    assert!(*execution.stop.borrow());
    assert!(manager.restore_waits_for_unblock(&chat.id, &execution.plan.id));
}

/// Why Shoal stopped the agent of `execution`, while it waits to restore.
async fn stop_reason(manager: &Manager, execution: &StartedExecution) -> Option<String> {
    let protection = manager.agent_overload(&execution.plan.id).await;
    protection.map(|protection| protection.reason)
}

/// Report a turn of the agent `execution` runs in `workspace`, ending now.
async fn turn(manager: &Manager, workspace: &Workspace, execution: &StartedExecution) {
    for state in [AgentState::Working, AgentState::Idle] {
        let id = Some(execution.plan.id.clone());
        (manager.set_agent_state(&SpaceSelector::id(&workspace.id), state, id, None))
            .await
            .unwrap();
    }
}

/// An agent the user is engaged with waits for them as long as
/// `[sleep] max_wait_minutes` allows without a reply time to go by, then
/// sleeps; a turn after that without them waits only seconds.
#[tokio::test]
async fn an_engaged_user_keeps_the_agent_until_they_stop_replying() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (workspace, execution) = agent(&manager, &repo.id, "engaged", IDLE).await;
    let earlier = UnixTime::now() - Duration::from_secs(100);
    (manager.record_interaction(&workspace.id, earlier))
        .await
        .unwrap();
    turn(&manager, &workspace, &execution).await;
    manager.stop_idle_agents(later()).await.unwrap();
    assert!(!*execution.stop.borrow(), "the user is engaged");
    let after_max_wait = UnixTime::now() + Duration::from_secs(31 * 60);
    manager.stop_idle_agents(after_max_wait).await.unwrap();
    assert!(*execution.stop.borrow());

    // A turn an update starts finds the user still away.
    let (workspace, execution) = agent(&manager, &repo.id, "updated", IDLE).await;
    (manager.record_interaction(&workspace.id, earlier))
        .await
        .unwrap();
    turn(&manager, &workspace, &execution).await;
    turn(&manager, &workspace, &execution).await;
    manager.stop_idle_agents(later()).await.unwrap();
    assert!(*execution.stop.borrow(), "the user stopped replying");
}

/// `[sleep] enabled = false` keeps idle agents running.
#[tokio::test]
async fn sleep_can_be_disabled() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    (manager.write_local_config(&repo.id, Some("[sleep]\nenabled = false\n".into())))
        .await
        .unwrap();
    let (_, execution) = agent(&manager, &repo.id, "idle", IDLE).await;
    manager.stop_idle_agents(later()).await.unwrap();
    assert!(!*execution.stop.borrow());
}

/// An asleep agent's timer keeps its waiting restore from counting as an
/// idle agent's, which keeps the workspace from cleanup, and makes the
/// restore due once it fires.
#[tokio::test]
async fn an_asleep_agents_timer_keeps_its_workspace_and_restores_it() {
    let (root, manager) = manager().await;
    manager.publish_config(crate::daemon::test_support::global_config(
        "[machine.overload.disk]\nenabled = false",
    ));
    let repo = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (workspace, execution) = agent(&manager, &repo.id, "sleeps", IDLE).await;
    let selector = SpaceSelector::id(&workspace.id);
    let at = UnixTime::now() + std::time::Duration::from_secs(20 * 60);
    let timer = shoal_protocol::sleep::WakeTimer { at, message: None };
    (manager.sleep(&selector, Some(Sleep::Turn), Some(timer)))
        .await
        .unwrap();
    manager.stop_idle_agents(later()).await.unwrap();
    assert!(*execution.stop.borrow());
    let agent = crate::agent::Agent::from("claude");
    manager
        .queue_restore(&execution.plan.id, &agent, &workspace.id)
        .await
        .unwrap();
    let _link = manager.agent_queue.link_restore(&execution.plan.id);
    let idle = manager.idle_restores(&workspace.id).await.unwrap();
    assert!(idle.is_empty(), "{idle:?}");
    let due = manager.restore_due(&workspace.id, &execution.plan.id);
    assert!(!due.await.unwrap());

    manager.fire_wake_timers(at).await.unwrap();
    let due = manager.restore_due(&workspace.id, &execution.plan.id);
    assert!(due.await.unwrap());
}

/// A light workspace agent that sleeps only stops its idle wakes.
#[tokio::test]
async fn a_light_agent_that_sleeps_keeps_running() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let config = "[modes.triage]\nweight = 'light'\n";
    (manager.write_local_config(&repo.id, Some(config.into())))
        .await
        .unwrap();
    let light = Setup {
        mode: "triage",
        ..IDLE
    };
    let (workspace, execution) = agent(&manager, &repo.id, "sleeps", light).await;
    let selector = SpaceSelector::id(&workspace.id);
    (manager.sleep(&selector, Some(Sleep::Turn), None))
        .await
        .unwrap();

    manager.stop_idle_agents(later()).await.unwrap();

    assert!(!*execution.stop.borrow());
}

/// Whether an idle agent the user is away from, with a warm prompt cache,
/// sleeps under the machine's `schedule`.
async fn warm_agent_sleeps(schedule: &str) -> bool {
    let (root, manager) = manager().await;
    let config = format!("[machine]\nschedule = \"{schedule}\"\n");
    manager.publish_config(crate::test_support::global_config(&config));
    let repo = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (workspace, execution) = agent(&manager, &repo.id, "warm", IDLE).await;
    cache_expires_in(&manager, &workspace, 600).await;
    manager.stop_idle_agents(later()).await.unwrap();
    *execution.stop.borrow()
}

#[tokio::test]
async fn a_warm_cache_keeps_no_idle_agent_under_the_cache_schedule() {
    assert!(warm_agent_sleeps("cache").await);
}

#[tokio::test]
async fn a_warm_cache_keeps_no_idle_agent_under_the_balanced_schedule() {
    assert!(warm_agent_sleeps("balanced").await);
}

#[tokio::test]
async fn a_warm_cache_keeps_no_idle_agent_under_the_speed_schedule() {
    assert!(warm_agent_sleeps("speed").await);
}

#[tokio::test]
async fn an_agent_the_user_is_away_from_sleeps_to_make_room_for_queued_work() {
    let (root, manager) = manager().await;
    manager.publish_config(crate::test_support::global_config(
        "[machine]\nmax_heavy_agents = 3\n[machine.overload.disk]\nenabled = false",
    ));
    let repo = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    // Sleep is off, so only making room puts agents to sleep.
    (manager.write_local_config(&repo.id, Some("[sleep]\nenabled = false\n".into())))
        .await
        .unwrap();
    let mut agents = Vec::new();
    for name in ["away", "urgent", "engaged"] {
        agents.push(agent(&manager, &repo.id, name, IDLE).await);
    }
    let [(_, away), (urgent, ranked_higher), (engaged, with_user)] = &agents[..] else {
        unreachable!()
    };
    manager
        .set_priority(None, &SpaceSelector::id(&urgent.id), Priority::URGENT)
        .await
        .unwrap();
    (manager.record_interaction(&engaged.id, UnixTime::now()))
        .await
        .unwrap();
    // Without queued work, nothing sleeps for room.
    manager.stop_idle_agents(later()).await.unwrap();
    assert!(!*away.stop.borrow());

    let queued = crate::daemon::agent_queue::tests::request(&manager, &repo, "later", false);
    let launch = manager.queue_add(queued.await).await.unwrap();
    assert_eq!(
        launch.entry.unwrap().state,
        crate::state::QueueState::Queued
    );
    manager.dispatch_queue().await.unwrap();
    let ended = turn_end(&manager, away).await;
    manager.stop_idle_agents(ended).await.unwrap();
    assert!(!*away.stop.borrow(), "its turn just ended");
    manager.stop_idle_agents(later()).await.unwrap();
    assert_eq!(
        stop_reason(&manager, away).await,
        Some(Protection::making_room().reason)
    );
    assert!(!*ranked_higher.stop.borrow(), "a higher priority keeps it");
    assert!(!*with_user.stop.borrow(), "the user is engaged with it");
}

/// An idle agent with a working agent below it stays running when queued
/// work waits for room, so a coordinator never sleeps while its children
/// work.
#[tokio::test]
async fn an_agent_with_a_working_agent_below_it_does_not_sleep_for_room() {
    let (root, manager) = manager().await;
    manager.publish_config(crate::test_support::global_config(
        "[machine]\nmax_heavy_agents = 2\n[machine.overload.disk]\nenabled = false",
    ));
    let repo = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    (manager.write_local_config(&repo.id, Some("[sleep]\nenabled = false\n".into())))
        .await
        .unwrap();
    let (parent, coordinator) = agent(&manager, &repo.id, "coordinator", IDLE).await;
    let working = Setup {
        state: AgentState::Working,
        ..IDLE
    };
    let (child, worker) = agent(&manager, &repo.id, "child", working).await;
    let (above, below) = (parent.id.clone(), child.id.clone());
    (manager.store)
        .run(move |db| crate::daemon::tree::set_parent(db, &below, Some(&above)))
        .await
        .unwrap();
    let queued = crate::daemon::agent_queue::tests::request(&manager, &repo, "later", false);
    manager.queue_add(queued.await).await.unwrap();
    manager.dispatch_queue().await.unwrap();

    manager.stop_idle_agents(later()).await.unwrap();
    assert!(!*coordinator.stop.borrow(), "its child works below it");
    assert!(!*worker.stop.borrow());
}

#[tokio::test]
async fn no_agent_sleeps_for_a_slot_the_machine_does_not_have() {
    let (root, manager) = manager().await;
    manager.publish_config(crate::test_support::global_config(
        "[machine]\nmax_heavy_agents = 0\n[machine.overload.disk]\nenabled = false",
    ));
    let repo = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    (manager.write_local_config(&repo.id, Some("[sleep]\nenabled = false\n".into())))
        .await
        .unwrap();
    let (_, away) = agent(&manager, &repo.id, "away", IDLE).await;
    let queued = crate::daemon::agent_queue::tests::request(&manager, &repo, "later", false);
    manager.queue_add(queued.await).await.unwrap();
    manager.dispatch_queue().await.unwrap();
    let entries = manager.queue_entries().await.unwrap();
    assert_eq!(
        (entries.iter().map(|entry| entry.held_by)).collect::<Vec<_>>(),
        [Some(crate::model::HeldBy::MaxHeavyAgents)]
    );
    manager.stop_idle_agents(later()).await.unwrap();
    assert!(!*away.stop.borrow());
}

/// The agent of a new workspace `name`, put to sleep as idle while the user
/// is away, its restore queued as its wrapper queues it, with disk space
/// left out of the restore's wait.
async fn asleep_while_away(
    manager: &Manager,
    repository_id: &RepositoryId,
    name: &str,
) -> (Workspace, StartedExecution) {
    manager.publish_config(crate::daemon::test_support::global_config(
        "[machine.overload.disk]\nenabled = false",
    ));
    let (workspace, execution) = agent(manager, repository_id, name, IDLE).await;
    manager.stop_idle_agents(later()).await.unwrap();
    assert!(*execution.stop.borrow());
    manager
        .queue_agent_restore(&execution.plan.id)
        .await
        .unwrap();
    (workspace, execution)
}

/// A comment on the PR of an agent asleep while the user is away restores
/// it, handed to its waiting wrapper over the wrapper's link.
#[tokio::test]
async fn a_pr_comment_restores_an_agent_asleep_while_away_through_its_wrapper() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (workspace, execution) = asleep_while_away(&manager, &repo.id, "away").await;
    let link = manager.agent_queue.link_restore(&execution.plan.id);
    manager.dispatch_queue().await.unwrap();
    assert_eq!(manager.agent_queue.restore_grant(&execution.plan.id), None);

    crate::daemon::links::wait::test_support::comment_on_watched_pr(&manager, &workspace.id).await;
    manager.dispatch_queue().await.unwrap();

    let admitted = manager.await_restore_admission(&link);
    let grant = tokio::time::timeout(Duration::from_secs(60), admitted).await;
    assert_eq!(
        grant.expect("the restore was admitted").unwrap(),
        crate::daemon::agent_queue::Grant::Update
    );
}

/// Once the wrapper of an agent asleep while the user is away is gone, a
/// comment on its PR has the daemon restore it in a new place.
#[tokio::test]
async fn a_pr_comment_restores_an_agent_asleep_while_away_whose_wrapper_is_gone() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (workspace, execution) = asleep_while_away(&manager, &repo.id, "away").await;
    // Its wrapper went without restoring it, and its execution ended.
    let ended = execution.plan.id.clone();
    (manager.store)
        .run(move |db| {
            db.execute("DELETE FROM executions WHERE id=?1", [ended])?;
            Ok(())
        })
        .await
        .unwrap();
    let state = |entries: Vec<crate::model::QueueEntry>| {
        let [entry] = entries.try_into().unwrap();
        (entry.state, entry.waits_for_update)
    };
    manager.dispatch_queue().await.unwrap();
    let queued = state(manager.queue_entries().await.unwrap());
    assert_eq!(queued, (crate::state::QueueState::Queued, true));

    crate::daemon::links::wait::test_support::comment_on_watched_pr(&manager, &workspace.id).await;
    manager.dispatch_queue().await.unwrap();

    let started = state(manager.queue_entries().await.unwrap());
    assert_eq!(started.0, crate::state::QueueState::Starting);
}
