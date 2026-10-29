use super::*;
use crate::{
    daemon::workspace::ExecutionKind,
    sessions::HostName,
    test_support::{manager, repository},
};
use anyhow::Context as _;

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

#[tokio::test]
async fn a_later_instance_under_a_reused_id_takes_its_record() -> Result<()> {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_string_lossy().into_owned().into(), None, None)
        .await?;
    let mut workspaces = Vec::new();
    for name in ["first", "second", "third"] {
        workspaces.push(
            manager
                .create_workspace(&repo.id, name.into(), None, None, None)
                .await?
                .id,
        );
    }
    let session = |instance: &str| Origin {
        instance: Some(instance.into()),
        ..Origin::created(HostName::Zmx, "shared".into())
    };
    let owner = async || -> Result<Option<String>> {
        Ok(manager
            .place_owner(HostName::Zmx, "shared")
            .await?
            .and_then(|(owner, _)| owner))
    };
    manager
        .record_owned_place(&workspaces[0], session("1:1"))
        .await?;
    // The session ended and another under its name began, before any agent
    // registered in it.
    manager
        .record_owned_place(&workspaces[1], session("2:2"))
        .await?;
    assert_eq!(owner().await?.as_ref(), Some(&workspaces[1]));
    // A late record of the same instance keeps the owner it has.
    manager
        .record_owned_place(&workspaces[2], session("2:2"))
        .await?;
    assert_eq!(owner().await?.as_ref(), Some(&workspaces[1]));
    Ok(())
}

#[tokio::test]
async fn closing_an_earlier_instance_keeps_the_record_that_replaced_it() -> Result<()> {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_string_lossy().into_owned().into(), None, None)
        .await?;
    let mut workspaces = Vec::new();
    for name in ["first", "second"] {
        workspaces.push(
            manager
                .create_workspace(&repo.id, name.into(), None, None, None)
                .await?
                .id,
        );
    }
    let session = |instance: &str| Origin {
        instance: Some(instance.into()),
        ..Origin::created(HostName::Zmx, "shared".into())
    };
    manager
        .record_owned_place(&workspaces[0], session("1:1"))
        .await?;
    // A close read the first record, then a later session's record replaced
    // it before the close forgot what it read.
    let (_, closed) = manager
        .place_owner(HostName::Zmx, "shared")
        .await?
        .context("the first record")?;
    manager
        .record_owned_place(&workspaces[1], session("2:2"))
        .await?;
    manager.forget_owned_place(closed).await?;
    let (owner, kept) = manager
        .place_owner(HostName::Zmx, "shared")
        .await?
        .context("the later record")?;
    assert_eq!(owner.as_ref(), Some(&workspaces[1]));
    assert_eq!(kept.instance.as_deref(), Some("2:2"));
    Ok(())
}
