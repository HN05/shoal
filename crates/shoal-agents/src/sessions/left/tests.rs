use shoal_core::sessions::{HostName, Origin};
use shoal_protocol::executions::{Stage, TrackedAgent};
use shoal_protocol::space_names::SpaceSelector;

use crate::execution::recovery::{self, Record, RecordExt as _};
use crate::executions::{ExecutionKind, ExecutionRequest};
use crate::places::ExecutionPlace;
use crate::sessions::tests::rex_fixture;

/// An agent that restores saved agents in another block than the ones they
/// stopped in leaves those blocks, which close; a block where something
/// still runs, where another saved agent waits, or that it runs in again
/// stays open.
#[tokio::test]
async fn a_restore_that_runs_elsewhere_closes_the_blocks_its_records_left() {
    let blocks = ["stopped", "busy", "waiting", "resumed"];
    let (fixture, workspace, host) = rex_fixture(&blocks).await;
    let (agents, daemon) = (&fixture.agents, &*fixture.daemon);
    let selector = SpaceSelector::id(&workspace.id);
    let agent = TrackedAgent::named("claude");
    let mut records = Vec::new();
    for block in blocks {
        host.add(block, workspace.id.as_str());
        let origin = Origin::created(HostName::Rex, block.into());
        let reason = Some("its session ended");
        let record = Record::new(&agent, reason, Stage::Stopped, Some(origin));
        let execution = format!("saved-{block}").into();
        let saved = recovery::save_stopped(&agents.paths, &workspace.id, &execution, record, None);
        if block != "waiting" {
            records.push(saved.unwrap());
        }
    }
    let at = |block: &str, agent: bool| ExecutionRequest {
        place: Some(ExecutionPlace {
            origin: Origin::created(HostName::Rex, block.into()),
            agent,
        }),
        agent,
        ..Default::default()
    };
    let kind = ExecutionKind::Command;
    let _nested =
        (agents.begin_execution_placed(daemon, &selector, None, kind, None, at("busy", false)))
            .await
            .unwrap();
    let restored =
        agents.begin_execution_placed(daemon, &selector, None, kind, None, at("resumed", true));
    let restored = restored.await.unwrap();

    let left = (agents.left_places(&workspace.id, &restored.plan.id, &records)).await;
    agents.close_left_places(daemon, &workspace.id, left).await;

    assert_eq!(host.closed(), ["stopped"]);
    let owned = agents
        .owned_places(Some(workspace.id.clone()))
        .await
        .unwrap();
    let owned: Vec<_> = (owned.into_iter())
        .map(|(_, place)| place.id().to_owned())
        .collect();
    assert_eq!(owned, ["busy", "waiting", "resumed"]);
}
