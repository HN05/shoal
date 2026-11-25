use super::{connected_agent, create, limit_globally, queue, register, started};
use crate::{
    daemon::{agents, manager::Manager},
    execution::recovery,
    model::{Asleep, QueueEntry},
};
use crate::{
    daemon::{
        test_support::{global_config, manager},
        workspace_files::Attacher,
    },
    execution::recovery::record_path,
    model::Workspace,
    state::QueueState::Queued,
};
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::assignments::Done;
use shoal_protocol::space_names::SpaceSelector;
use std::collections::BTreeMap;

/// The variables the restores of these tests start with.
fn herdr() -> BTreeMap<String, String> {
    BTreeMap::from([("HERDR_ENV".into(), "1".into())])
}

/// Save the record of an agent stopped as `execution` in the space `id`.
fn stop(manager: &Manager, id: &SpaceId, execution: &str, record: &str) {
    let path = record_path(&manager.paths, id, &execution.into());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, record).unwrap();
}

const STOPPED: &str = r#"{"agent":"claude","mode":"worker","stage":"stopped"}"#;

/// Save the record and restore request a signal stop leaves.
fn signal_stop(manager: &Manager, id: &SpaceId, execution: &str) {
    stop(manager, id, execution, STOPPED);
    let request = record_path(&manager.paths, id, &execution.into())
        .with_file_name(format!("{execution}.restore-request.json"));
    std::fs::write(request, r#"{"HERDR_ENV":"1"}"#).unwrap();
}

/// The variables the restore requested for `execution` starts with.
fn requested(
    manager: &Manager,
    workspace: &Workspace,
    execution: &str,
) -> Option<BTreeMap<String, String>> {
    recovery::restore_requests(&manager.paths, &workspace.id)
        .unwrap()
        .into_iter()
        .find_map(|(id, env)| (id == execution).then_some(env))
}

#[tokio::test]
async fn signal_stops_queue_their_restore_once_the_execution_has_ended() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    manager.publish_config(global_config(
        "[machine]\nmax_heavy_agents = 1000\n[machine.overload.disk]\nenabled = false",
    ));
    let open = create(&manager, &repo, "open").await;
    let done = create(&manager, &repo, "done").await;
    let (running, _connection) = connected_agent(&manager, &open).await;
    signal_stop(&manager, &open.id, running.as_str());
    signal_stop(&manager, &open.id, "ended");
    stop(&manager, &open.id, "manual", STOPPED);
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&done.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    signal_stop(&manager, &done.id, "finished");

    manager.agents.dispatch_queue(&*manager).await.unwrap();
    let entries = manager.agents.queue_entries(&*manager).await.unwrap();
    // Without a limit the daemon starts it at once, as its terminal would.
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].execution.as_ref().map(ExecutionId::as_str),
        Some("ended")
    );
    assert!(started(&entries[0].id));
    assert_eq!(requested(&manager, &open, "ended"), None);
    // The running execution, like a done workspace's agent, keeps its request.
    assert_eq!(requested(&manager, &open, running.as_str()), Some(herdr()));
    assert_eq!(requested(&manager, &done, "finished"), Some(herdr()));
}

/// The record of an agent stopped between turns.
const IDLE: &str = r#"{"agent":"claude","mode":"worker","stage":"stopped","turn":"idle"}"#;

/// The queued entry restoring `execution`.
async fn entry(manager: &Manager, execution: &str) -> QueueEntry {
    manager
        .agents
        .queue_entries(manager)
        .await
        .unwrap()
        .into_iter()
        .find(|entry| entry.execution.as_ref().map(ExecutionId::as_str) == Some(execution))
        .unwrap()
}

/// Why the agent of the workspace `name` is asleep and what wakes it.
async fn asleep(manager: &Manager, name: &str) -> Option<Asleep> {
    let id = manager.named(name).await;
    manager.spaces.workspace(&id).await.unwrap().asleep
}

/// Restoring a done chat's agent withdraws the chat's completion, as it
/// does a workspace's, so completion cleanup leaves the restored chat.
#[tokio::test]
async fn queueing_a_done_chats_restore_withdraws_its_completion() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let chat = crate::daemon::test_support::chat(&manager, "talk", "chat", Some(&repo), None).await;
    let selector = SpaceSelector::id(&chat.id);
    (manager
        .spaces
        .mark_space_done(&*manager, &selector, Done::default(), &Attacher::User))
    .await
    .unwrap();
    stop(&manager, &chat.id, "stopped-agent", STOPPED);

    (manager
        .agents
        .queue_stopped_restore(&*manager, &selector, &"stopped-agent".into(), herdr()))
    .await
    .unwrap();

    assert_eq!(manager.spaces.completion(&chat.id).await.unwrap(), None);
}

#[tokio::test]
async fn a_signal_stopped_idle_agent_waits_for_an_update_unless_resumed() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    let mut workspaces = Vec::new();
    for (name, record) in [("idle", IDLE), ("resumed", IDLE), ("working", STOPPED)] {
        let workspace = create(&manager, &repo, name).await;
        let execution = ExecutionId::from(format!("{name}-agent"));
        signal_stop(&manager, &workspace.id, execution.as_str());
        stop(&manager, &workspace.id, execution.as_str(), record);
        workspaces.push(workspace);
    }
    // The idle agents hold no slot, so the one stopped mid-turn takes it.
    manager.agents.dispatch_queue(&*manager).await.unwrap();
    let working = entry(&manager, "working-agent").await;
    assert!(started(&working.id) && !working.waits_for_update);
    for execution in ["idle-agent", "resumed-agent"] {
        let waiting = entry(&manager, execution).await;
        assert!(waiting.waits_for_update && waiting.state == Queued);
    }

    // Resuming one keeps its place, and it waits only for a slot.
    manager
        .agents
        .queue_stopped_restore(
            &*manager,
            &manager.named("resumed").await,
            &ExecutionId::from("resumed-agent"),
            herdr(),
        )
        .await
        .unwrap();
    assert!(!entry(&manager, "resumed-agent").await.waits_for_update);
    // Shoal brings the idle one back on its own; the resumed one waits for
    // its slot because the user asked for it.
    assert_eq!(
        asleep(&manager, "idle")
            .await
            .map(|asleep| asleep.wakes_when),
        Some(agents::WAKES_ON_UPDATE.into())
    );
    assert_eq!(asleep(&manager, "resumed").await, None);
    manager
        .agents
        .consume_queue_entry(&working.id, &workspaces[2].id)
        .await;
    std::fs::remove_file(record_path(
        &manager.paths,
        &workspaces[2].id,
        &ExecutionId::from("working-agent"),
    ))
    .unwrap();
    manager.agents.dispatch_queue(&*manager).await.unwrap();
    let resumed = entry(&manager, "resumed-agent").await;
    assert!(started(&resumed.id));
    assert_eq!(entry(&manager, "idle-agent").await.state, Queued);

    // An update for the idle agent restores it once a slot is free.
    manager
        .agents
        .consume_queue_entry(&resumed.id, &workspaces[1].id)
        .await;
    std::fs::remove_file(record_path(
        &manager.paths,
        &workspaces[1].id,
        &ExecutionId::from("resumed-agent"),
    ))
    .unwrap();
    manager
        .coordination
        .send_space_message(
            &*manager,
            None,
            &manager.named("idle").await,
            "Review requested".into(),
            Vec::new(),
        )
        .await
        .unwrap();
    manager.agents.dispatch_queue(&*manager).await.unwrap();
    assert!(started(&entry(&manager, "idle-agent").await.id));
}

#[tokio::test]
async fn a_queued_restore_of_an_idle_agent_keeps_its_workspace_from_no_cleanup() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let idle = create(&manager, &repo, "idle").await;
    signal_stop(&manager, &idle.id, "idle-agent");
    stop(&manager, &idle.id, "idle-agent", IDLE);
    let working = create(&manager, &repo, "working").await;
    signal_stop(&manager, &working.id, "working-agent");
    manager.agents.dispatch_queue(&*manager).await.unwrap();
    assert!(entry(&manager, "idle-agent").await.waits_for_update);
    assert!(
        manager
            .spaces
            .cleanup_candidacy(&*manager, &idle.id)
            .await
            .unwrap()
            .is_ok()
    );
    assert_eq!(
        manager
            .spaces
            .cleanup_candidacy(&*manager, &working.id)
            .await
            .unwrap(),
        Err(crate::model::CleanupReason::Recovering)
    );

    // An update gives the agent work, though no reading has found free disk
    // space for the new configuration, which holds its restore back.
    manager.publish_config(global_config(""));
    manager
        .coordination
        .send_space_message(
            &*manager,
            None,
            &manager.named("idle").await,
            "Review requested".into(),
            Vec::new(),
        )
        .await
        .unwrap();
    manager.agents.dispatch_queue(&*manager).await.unwrap();
    assert_eq!(entry(&manager, "idle-agent").await.state, Queued);
    assert_eq!(
        manager
            .spaces
            .cleanup_candidacy(&*manager, &idle.id)
            .await
            .unwrap(),
        Err(crate::model::CleanupReason::Recovering)
    );
}

#[tokio::test]
async fn a_stop_or_completion_keeps_stopped_agents_down() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    limit_globally(&manager, 1);
    let busy = create(&manager, &repo, "busy").await;
    let (_running, _connection) = connected_agent(&manager, &busy).await;
    // A signal stop's request that dispatch has not queued yet.
    let signalled = create(&manager, &repo, "signalled").await;
    signal_stop(&manager, &signalled.id, "requested");
    manager
        .agents
        .stop_space(
            &*manager,
            &manager.named("signalled").await,
            crate::daemon::executions::StopRequest::manual(),
        )
        .await
        .unwrap();
    manager.agents.dispatch_queue(&*manager).await.unwrap();
    assert!(queue(&manager).await.is_empty());
    assert_eq!(requested(&manager, &signalled, "requested"), None);
    assert!(
        record_path(
            &manager.paths,
            &signalled.id,
            &ExecutionId::from("requested")
        )
        .exists()
    );

    // A restore waiting when its assignment is done is cancelled.
    let finished = create(&manager, &repo, "finished").await;
    stop(&manager, &finished.id, "waiting", STOPPED);
    manager
        .agents
        .queue_stopped_restore(
            &*manager,
            &manager.named("finished").await,
            &ExecutionId::from("waiting"),
            herdr(),
        )
        .await
        .unwrap();
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&finished.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    assert!(queue(&manager).await.is_empty());
    assert!(record_path(&manager.paths, &finished.id, &ExecutionId::from("waiting")).exists());
}
