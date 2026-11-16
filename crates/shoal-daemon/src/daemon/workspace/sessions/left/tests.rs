use super::super::tests::rex_fixture;
use crate::{
    daemon::workspace::{ExecutionKind, ExecutionRequest, places::ExecutionPlace},
    execution::{
        TrackedAgent,
        recovery::{self, Record, RecordExt as _},
    },
    sessions::{HostName, Origin},
};
use anyhow::Result;
use shoal_protocol::{executions::Stage, space_names::SpaceSelector};

/// A restored agent that runs in another block than the one it stopped in
/// leaves that block, which closes; a block where something still runs, or
/// the one it runs in again, stays open.
#[tokio::test]
async fn an_agent_that_runs_elsewhere_closes_the_blocks_its_saved_agents_left() -> Result<()> {
    let blocks = ["stopped", "busy", "resumed"];
    let (_root, manager, workspace, host) = rex_fixture(&blocks).await?;
    let selector = SpaceSelector::id(&workspace.id);
    let agent = TrackedAgent::named("claude");
    for block in blocks {
        host.add(block, workspace.id.as_str())?;
        let origin = Origin::created(HostName::Rex, block.into());
        let record = Record::new(
            &agent,
            Some("its session ended"),
            Stage::Stopped,
            Some(origin),
        );
        let execution = format!("saved-{block}").into();
        recovery::save_stopped(&manager.paths, &workspace.id, &execution, record, None)?;
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
        (manager.begin_execution_placed(&selector, None, kind, None, at("busy", false))).await?;
    let restored =
        (manager.begin_execution_placed(&selector, None, kind, None, at("resumed", true))).await?;

    let left = manager.left_places(&workspace.id, &restored.plan.id).await;
    manager.close_left_places(&workspace.id, left).await;

    assert_eq!(host.closed()?, ["stopped"]);
    let owned: Vec<_> = (manager.owned_places(Some(workspace.id.clone())).await?)
        .into_iter()
        .map(|(_, place)| place.id().to_owned())
        .collect();
    assert_eq!(owned, ["busy", "resumed"]);
    Ok(())
}
