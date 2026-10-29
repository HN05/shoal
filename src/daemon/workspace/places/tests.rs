use super::*;
use crate::{
    daemon::workspace::ExecutionKind,
    sessions::HostName,
    test_support::{manager, repository},
};

#[tokio::test]
async fn an_execution_reports_where_it_runs() -> Result<()> {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_string_lossy().into_owned().into(), None, None)
        .await?;
    let workspace = manager
        .create_workspace(&repo.id, "placed".into(), None, None, None)
        .await?;
    let mut origin = Origin::created(HostName::Rex, "block:1".into());
    origin.owned = false;
    origin.group = Some("session:1".into());
    let _started = manager
        .begin_execution_placed(
            &workspace.id,
            None,
            ExecutionKind::Command,
            None,
            None,
            Some(crate::daemon::workspace::places::ExecutionPlace {
                origin: origin.clone(),
                agent: true,
            }),
        )
        .await?;
    let executions = manager.inspect_workspace(&workspace.id).await?.executions;
    assert_eq!(executions.len(), 1);
    assert_eq!(executions[0].origin, Some(origin));
    Ok(())
}
