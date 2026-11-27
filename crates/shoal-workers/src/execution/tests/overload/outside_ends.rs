//! An idle agent that a signal Shoal did not send ends sleeps: its restore
//! waits in the agent queue, and an update for it wakes it.
use super::*;
use crate::state::AgentState;
use shoal_daemon::test_daemon::overload::queue_merge_conflict;
use shoal_protocol::events::{EventItem, EventKind};
use shoal_protocol::stop_causes::{EndSignal, SignalTarget, StopCause};

/// An agent that a signal ends, since it traps none.
#[tokio::test]
#[ignore = "isolated tracked wrapper launched by outside end tests"]
async fn untrapping_agent_child() {
    if std::env::var_os("SHOAL_TEST_HELPER").is_none() {
        return;
    }
    run_tracked_agent("touch started; while :; do sleep 1; done").await;
}

/// The wrapper of an idle agent that a signal ends, and its execution.
struct Idle {
    launched: tokio::task::JoinHandle<Result<i32>>,
    execution: crate::model::Execution,
}

/// Launch an agent that a signal ends and report it idle between turns.
async fn idle_agent(manager: &Manager, workspace: &crate::model::Workspace) -> Idle {
    let test = "execution::tests::overload::outside_ends::untrapping_agent_child";
    let launched = launch_child(manager, workspace, test);
    wait_started(manager, workspace).await;
    let execution = manager
        .spaces
        .inspect_workspace(manager, &SpaceSelector::id(&workspace.id))
        .await
        .unwrap()
        .executions
        .remove(0);
    let selector = SpaceSelector::id(&workspace.id);
    let id = Some(execution.id.clone());
    (manager.agents)
        .set_agent_state(manager, &selector, AgentState::Idle, id, None)
        .await
        .unwrap();
    Idle {
        launched,
        execution,
    }
}

/// Send `signal` to the agent's process group, leaving its wrapper alone.
fn signal_agent(execution: &crate::model::Execution, signal: i32) {
    let group = execution.group_id.unwrap() as i32;
    // SAFETY: kill only sends a signal, to the agent group the test started.
    unsafe {
        libc::kill(-group, signal);
    }
}

fn signal_wrapper(execution: &crate::model::Execution, signal: i32) {
    let wrapper = execution.wrapper.as_ref().unwrap();
    crate::process::identity::signal(wrapper, signal).unwrap();
}

/// What the space's log names as the end of `execution`'s agent.
async fn stop_cause(manager: &Manager, execution: &ExecutionId) -> Option<StopCause> {
    let events = manager.space_events(None, 100).await.unwrap();
    events.iter().find_map(|item| match item {
        EventItem::Event(event) if event.details.kind() == EventKind::AgentStopped => {
            let agent = event.details.event.agent()?;
            (agent.execution_id.as_ref() == Some(execution)).then_some(agent.cause)?
        }
        _ => None,
    })
}

/// The restore of `execution`'s agent once the daemon queued it from the
/// request its wrapper left.
async fn requested_restore(
    manager: &Manager,
    workspace: &crate::model::Workspace,
    execution: &ExecutionId,
) -> crate::model::QueueEntry {
    use shoal_core::execution::recovery::restore_requests;
    bounded(async {
        loop {
            let requested = restore_requests(&manager.paths, &workspace.id).unwrap();
            let entries = manager.agents.queue_entries(manager).await.unwrap();
            let restore =
                (entries.into_iter()).find(|entry| entry.execution.as_ref() == Some(execution));
            if let Some(restore) = restore.filter(|_| requested.is_empty()) {
                return restore;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
}

/// Once the agent's wrapper ended, its restore waits in the agent queue
/// until an update arrives for it, which starts it.
async fn sleeps_until_an_update(
    manager: &Manager,
    workspace: &crate::model::Workspace,
    idle: Idle,
) {
    bounded(idle.launched).await.unwrap().unwrap();
    let restore = requested_restore(manager, workspace, &idle.execution.id).await;
    assert!(restore.waits_for_update, "{restore:?}");
    let execution = idle.execution.id.as_str();
    // The fixture runs no disk monitor; a reading finds the space free.
    let generation = manager.config_state.generation();
    manager.config_state.publish_disk_reading(Some(generation));
    queue_merge_conflict(manager, workspace).await;
    starting_restore(manager, execution).await;
}

#[tokio::test]
async fn an_idle_agent_a_signal_to_it_alone_ends_sleeps_until_an_update() {
    let (_root, manager, workspace, serving) = fixture().await;
    let idle = idle_agent(&manager, &workspace).await;
    signal_agent(&idle.execution, libc::SIGTERM);
    let execution = idle.execution.id.clone();
    sleeps_until_an_update(&manager, &workspace, idle).await;
    let cause = StopCause::Signal {
        signal: EndSignal::Terminate,
        reached: SignalTarget::Agent,
    };
    assert_eq!(stop_cause(&manager, &execution).await, Some(cause));
    serving.abort();
}

#[tokio::test]
async fn an_idle_agent_a_signal_to_its_wrapper_and_it_ends_sleeps_until_an_update() {
    let (_root, manager, workspace, serving) = fixture().await;
    let idle = idle_agent(&manager, &workspace).await;
    signal_wrapper(&idle.execution, libc::SIGTERM);
    signal_agent(&idle.execution, libc::SIGTERM);
    let execution = idle.execution.id.clone();
    sleeps_until_an_update(&manager, &workspace, idle).await;
    let cause = stop_cause(&manager, &execution).await;
    assert!(
        matches!(
            cause,
            Some(StopCause::Signal {
                signal: EndSignal::Terminate,
                ..
            })
        ),
        "{cause:?}"
    );
    serving.abort();
}

#[tokio::test]
async fn an_idle_agent_a_kill_ends_sleeps_until_an_update() {
    let (_root, manager, workspace, serving) = fixture().await;
    let idle = idle_agent(&manager, &workspace).await;
    signal_agent(&idle.execution, libc::SIGKILL);
    let execution = idle.execution.id.clone();
    sleeps_until_an_update(&manager, &workspace, idle).await;
    let cause = StopCause::Signal {
        signal: EndSignal::Kill,
        reached: SignalTarget::Agent,
    };
    assert_eq!(stop_cause(&manager, &execution).await, Some(cause));
    serving.abort();
}

/// A wrapper that waits to restore its asleep agent leaves the restore to
/// the agent queue when a signal ends it.
#[tokio::test]
async fn an_asleep_agent_whose_waiting_wrapper_a_signal_ends_sleeps_until_an_update() {
    let (_root, manager, workspace, serving) = fixture().await;
    let idle = idle_agent(&manager, &workspace).await;
    let monitor = manager.gates.operation().await;
    assert!(
        manager
            .agents
            .stop_for_overload(&*manager, shoal_daemon::test_daemon::Pressure::Memory)
            .await
    );
    drop(monitor);
    wait_paused(&manager, &workspace).await;
    wait_restore_queued(&manager).await;
    signal_wrapper(&idle.execution, libc::SIGTERM);
    sleeps_until_an_update(&manager, &workspace, idle).await;
    serving.abort();
}
