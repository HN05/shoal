use std::collections::HashSet;

use rusqlite::params;

use crate::client::{execution_ids::ExecutionId, space_names::SpaceSelector};
use crate::{
    daemon::{
        test_support::{global_config, manager},
        workspace::agent_queue::tests::{add, create, register, request},
    },
    execution::recovery::record_path,
    model::QueueRequest,
    state::{ExecutionState, QueueState},
};

#[tokio::test]
async fn a_light_agent_holds_no_slot_and_starts_beside_full_limits() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    manager.publish_config(global_config(
        "[machine]\nmax_agents = 1\n[machine.overload.disk]\nenabled = false\n\
         [modes.triage]\nweight = 'light'\n",
    ));
    let workspace = create(&manager, &repo, "busy").await;
    let id = workspace.id.clone();
    // A heavy agent fills the one slot; a light one beside it takes none.
    manager
        .store
        .run(move |db| {
            for (execution, mode) in [("heavy", "worker"), ("light", "triage")] {
                db.execute(
                    "INSERT INTO executions(id,space_id,state,agent,mode) VALUES (?1,?2,?3,'claude',?4)",
                    params![execution, id, ExecutionState::Unknown, mode],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        manager.occupied_slots(&HashSet::new()).await.unwrap().len(),
        1
    );
    let triage = |name| QueueRequest {
        mode: Some("triage".into()),
        ..request(&repo, name, false)
    };
    assert!(!add(&manager, &repo, "heavy").await.start);
    // A light launch of a new workspace starts at once.
    assert!(manager.queue_add(triage("new")).await.unwrap().start);
    // One in an existing workspace is admitted past the waiting heavy one.
    let free = create(&manager, &repo, "free").await;
    let admission = manager
        .queue_add(QueueRequest {
            workspace: Some(SpaceSelector::id(&free.id)),
            ..triage("existing")
        })
        .await
        .unwrap();
    assert_eq!(admission.entry.unwrap().state, QueueState::Starting);
}

#[tokio::test]
async fn a_stopped_light_agent_waiting_for_its_restore_holds_no_slot() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    manager.publish_config(global_config("[modes.triage]\nweight = 'light'\n"));
    let workspace = create(&manager, &repo, "stopped").await;
    // Stopped agents keep only their saved records, each with its mode.
    for (execution, mode) in [("heavy", "worker"), ("light", "triage")] {
        let execution = ExecutionId::from(execution);
        let record = record_path(&manager.paths, &workspace.id, &execution);
        std::fs::create_dir_all(record.parent().unwrap()).unwrap();
        let saved = serde_json::json!({"agent": "claude", "mode": mode});
        std::fs::write(&record, saved.to_string()).unwrap();
    }
    assert_eq!(
        manager.occupied_slots(&HashSet::new()).await.unwrap().len(),
        1
    );
    let light = manager.light().await.unwrap();
    assert!(light.execution(&"light".into()) && !light.execution(&"heavy".into()));
}
