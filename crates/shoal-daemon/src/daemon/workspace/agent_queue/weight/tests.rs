use std::collections::HashSet;

use rusqlite::params;

use crate::{
    daemon::{
        test_support::{global_config, manager},
        workspace::agent_queue::tests::{add, admitted, create, launch_in, register},
    },
    execution::recovery::record_path,
    model::QueueRequest,
    state::{ExecutionState, QueueState},
};
use shoal_ids::execution_ids::ExecutionId;

#[tokio::test]
async fn a_light_agent_holds_no_slot_and_starts_beside_full_limits() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    manager.publish_config(global_config(
        "[machine]\nmax_heavy_agents = 1\n[machine.overload.disk]\nenabled = false\n\
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
        manager
            .occupied_slots(&HashSet::new(), &manager.light().await.unwrap())
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(!admitted(&add(&manager, &repo, "heavy").await));
    // A light launch is admitted past the waiting heavy one.
    let free = create(&manager, &repo, "free").await;
    let admission = manager
        .queue_add(QueueRequest {
            mode: Some("triage".into()),
            ..launch_in(&free, false)
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
    // Stopped agents keep only their saved records, each with its mode; one
    // that does not read counts as heavy.
    let saved = |mode| serde_json::json!({"agent": "claude", "mode": mode}).to_string();
    for (execution, record) in [
        ("heavy", saved("worker")),
        ("light", saved("triage")),
        ("unreadable", "{".into()),
    ] {
        let execution = ExecutionId::from(execution);
        let path = record_path(&manager.paths, &workspace.id, &execution);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, record).unwrap();
    }
    let light = manager.light().await.unwrap();
    let heavy = ["heavy", "unreadable"].map(|id| !light.execution(&id.into()));
    assert!(light.execution(&"light".into()) && heavy == [true, true]);
    let slots = manager.occupied_slots(&HashSet::new(), &light).await;
    assert_eq!(slots.unwrap().len(), 2);
}
