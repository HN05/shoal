use crate::{
    daemon::{
        executions::ExecutionKind,
        test_support::{manager, repository},
    },
    sessions::HostName,
};
use anyhow::Result;
use shoal_core::sessions::Origin;
use shoal_protocol::space_names::SpaceSelector;

#[tokio::test]
async fn an_execution_reports_where_it_runs() -> Result<()> {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_string_lossy().into_owned().into(), None, None)
        .await?;
    let workspace = manager
        .create_workspace(&repo.id, "placed".into(), None, None, None)
        .await?;
    let mut origin = Origin::created(HostName::Rex, "block:1".into());
    origin.owned = false;
    origin.group = Some("session:1".into());
    let _started = manager
        .agents
        .begin_execution_placed(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            None,
            ExecutionKind::Command,
            None,
            crate::daemon::executions::ExecutionRequest {
                place: Some(crate::daemon::places::ExecutionPlace {
                    origin: origin.clone(),
                    agent: true,
                }),
                ..Default::default()
            },
        )
        .await?;
    let executions = manager
        .inspect_workspace(&SpaceSelector::id(&workspace.id))
        .await?
        .executions;
    assert_eq!(executions.len(), 1);
    assert_eq!(executions[0].origin, Some(origin));
    Ok(())
}
