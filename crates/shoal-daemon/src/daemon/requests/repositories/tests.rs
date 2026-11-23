//! Repository requests from a scoped caller.
use super::*;
use crate::daemon::scope::Access;
use crate::daemon::test_support::manager;
use shoal_ids::space_ids::SpaceId;

/// A process here cannot carry commits into a repository, which would
/// register its origin; only another machine's start does.
#[tokio::test]
async fn a_scoped_process_here_carries_no_commits() {
    let (_temp, manager) = manager().await;
    let caller = Caller {
        space_id: SpaceId::generate(),
        execution: None,
        granted: None,
        access: Access::Full,
        vouched: None,
    };
    let mut method = RepositoryMethod::ReceiveCommits {
        origin: "https://example.test/team/app".into(),
        commit: "0123456789abcdef0123456789abcdef01234567".into(),
        branch: "main".into(),
    };
    assert!(method.scope(&manager, &caller).await.is_err());
    assert!(manager.repositories.list().await.unwrap().is_empty());
}

/// A process whose `admin` permission is not `direct` reads nothing of
/// what removing its own repository would take: every space of it.
#[tokio::test]
async fn a_scoped_process_without_admin_previews_no_removal() {
    let (root, manager) = manager().await;
    let path = crate::daemon::test_support::repository(root.path(), "repo");
    let repo = (manager.repositories)
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "own".into(), None, None, None)
        .await
        .unwrap();
    let caller = Caller {
        space_id: workspace.id.clone(),
        execution: None,
        granted: None,
        access: Access::Full,
        vouched: None,
    };
    let mut method = RepositoryMethod::PreviewRepositoryRemoval {
        repository: (&repo.id).into(),
        machine: None,
    };
    let refused = method.scope(&manager, &caller).await.unwrap_err();
    assert!(format!("{refused:#}").contains("admin"), "{refused:#}");
}
