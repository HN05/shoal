use crate::execution::recovery::LastSession;
use crate::execution::recovery::LastSessionExt;
use crate::stand_in::Fixture;
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::executions::TrackedAgent;
use shoal_protocol::space_names::SpaceSelector;
use std::sync::Arc;

#[tokio::test]
async fn only_stopped_agents_or_commands_are_unfinished_work() {
    let fixture = Arc::new(Fixture::new().await);
    let mut ids = Vec::new();
    for name in ["stopped", "exited"] {
        let workspace = fixture.workspace(name).await;
        ids.push(workspace.id);
    }
    let (stopped, exited) = (&ids[0], &ids[1]);
    let agent = TrackedAgent::named("claude");
    let execution = ExecutionId::from("agent");
    crate::execution::recovery::save_record(
        &fixture.agents.paths,
        stopped,
        &execution,
        &agent,
        None,
        None,
    )
    .unwrap();
    LastSession::save(&fixture.agents.paths, exited, &agent, &execution, None).unwrap();

    let summaries = fixture
        .agents
        .saved_summaries(&*fixture.daemon, None)
        .await
        .unwrap();
    let summarized = summaries
        .iter()
        .map(|summary| (&summary.space, summary.unfinished, summary.agents.len()));
    let (stopped_one, exited_one) = ((stopped, true, 1), (exited, false, 1));
    let mut expected = vec![stopped_one, exited_one];
    expected.sort();
    let mut summarized = summarized.collect::<Vec<_>>();
    summarized.sort();
    assert_eq!(summarized, expected);
    let saved = fixture
        .agents
        .saved_work(&*fixture.daemon, &SpaceSelector::id(stopped))
        .await
        .unwrap();
    let agents = saved
        .agents
        .iter()
        .map(|agent| (&agent.id, agent.agent.as_ref()));
    assert_eq!(
        agents.collect::<Vec<_>>(),
        [(&execution, Some(&"claude".into()))]
    );
    let saved = fixture
        .agents
        .saved_work(&*fixture.daemon, &SpaceSelector::id(exited))
        .await
        .unwrap();
    assert!(saved.agents.is_empty() && saved.sessions.len() == 1);
}
