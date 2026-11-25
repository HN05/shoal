use crate::execution::recovery::record_path;

use super::*;

#[tokio::test]
async fn a_light_agent_holds_no_slot_and_starts_beside_full_limits() {
    let fixture = Fixture::new().await;
    fixture.configure(
        "[machine]\nmax_heavy_agents = 1\n[machine.overload.disk]\nenabled = false\n\
         [modes.triage]\nweight = 'light'\n",
    );
    let daemon = &*fixture.daemon;
    let workspace = fixture.workspace("busy").await;
    let id = workspace.id.clone();
    // A heavy agent fills the one slot; a light one beside it takes none.
    (fixture.agents.store)
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
    let light = fixture.agents.light(daemon).await.unwrap();
    let slots = (fixture
        .agents
        .occupied_slots(daemon, &HashSet::new(), &light))
    .await;
    assert_eq!(slots.unwrap().len(), 1);
    assert!(!admitted(&add(&fixture, "heavy").await));
    // A light launch is admitted past the waiting heavy one.
    let free = fixture.workspace("free").await;
    let request = QueueRequest {
        mode: Some("triage".into()),
        ..launch_in(&free, false)
    };
    let admission = fixture.agents.queue_add(daemon, request).await.unwrap();
    assert_eq!(admission.entry.unwrap().state, QueueState::Starting);
}

#[tokio::test]
async fn a_stopped_light_agent_waiting_for_its_restore_holds_no_slot() {
    let fixture = Fixture::new().await;
    fixture.configure("[modes.triage]\nweight = 'light'\n");
    let daemon = &*fixture.daemon;
    let workspace = fixture.workspace("stopped").await;
    // Stopped agents keep only their saved records, each with its mode; one
    // that does not read counts as heavy.
    let saved = |mode| serde_json::json!({"agent": "claude", "mode": mode}).to_string();
    for (execution, record) in [
        ("heavy", saved("worker")),
        ("light", saved("triage")),
        ("unreadable", "{".into()),
    ] {
        let execution = ExecutionId::from(execution);
        let path = record_path(&fixture.agents.paths, &workspace.id, &execution);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, record).unwrap();
    }
    let light = fixture.agents.light(daemon).await.unwrap();
    let heavy = ["heavy", "unreadable"].map(|id| !light.execution(&id.into()));
    assert!(light.execution(&"light".into()) && heavy == [true, true]);
    let slots = (fixture
        .agents
        .occupied_slots(daemon, &HashSet::new(), &light))
    .await;
    assert_eq!(slots.unwrap().len(), 2);
}
