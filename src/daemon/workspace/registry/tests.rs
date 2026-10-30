use crate::test_support::{manager, repository};

#[tokio::test]
async fn a_contribution_clones_a_url_and_keeps_an_existing_registration() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let source = path.to_str().unwrap().to_owned();
    let error = manager
        .register_contribution(source.clone(), None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("local path"), "{error:#}");
    let registered = manager
        .register_repository(source.as_str().into(), None, None)
        .await
        .unwrap();
    let again = manager.register_contribution(source, None).await.unwrap();
    assert_eq!(again.id, registered.id);
    assert!(!again.contribution);
}

/// A canonical temporary directory whose name Spotlight does not skip, and
/// an excluded `root_dir` named `shoal` in it.
fn excluded_root() -> (tempfile::TempDir, std::path::PathBuf, super::ConfiguredRoot) {
    let temp = tempfile::Builder::new()
        .prefix("registry")
        .tempdir()
        .unwrap();
    let parent = std::fs::canonicalize(temp.path()).unwrap();
    let root = super::ConfiguredRoot {
        path: parent.join("shoal"),
        layout: crate::daemon::workspace::spotlight::Layout::Excluded,
    };
    (temp, parent, root)
}

#[tokio::test]
async fn an_excluded_root_refuses_a_sibling_that_resolves_into_state() {
    let (_temp, parent, root) = excluded_root();
    let state = parent.join("state");
    std::fs::create_dir(&state).unwrap();
    std::os::unix::fs::symlink(&state, parent.join(".shoal.noindex")).unwrap();
    let error = super::prepare_root(&root, &state, &nothing(), None)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("inside Shoal's state directory"),
        "{error:#}"
    );
    assert!(!parent.join("shoal").exists());
}

#[tokio::test]
async fn an_excluded_root_refuses_a_recorded_repository_directory() {
    let (_temp, parent, root) = excluded_root();
    std::fs::create_dir(&root.path).unwrap();
    let recorded = crate::model::Repository {
        id: "recorded".into(),
        path: parent.join("elsewhere/checkout"),
        source: "https://example.invalid/recorded.git".into(),
        last_used: 1,
        name: None,
        workspaces_dir: Some(root.path.clone()),
        contribution: false,
    };
    let error = super::prepare_root(
        &root,
        &parent.join("state"),
        &super::Recorded {
            repositories: &[recorded],
            workspaces: &[],
        },
        None,
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("inside the repository directory"),
        "{error:#}"
    );
    assert!(std::fs::symlink_metadata(&root.path).unwrap().is_dir());
}

#[tokio::test]
async fn registration_keeps_a_checkout_in_a_root_spotlight_indexes() {
    let (_temp, parent, mut root) = excluded_root();
    root.layout = crate::daemon::workspace::spotlight::Layout::AsFound;
    let project = root.path.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let checkout = repository(&project, "checkout");
    let (path, directory) = super::local_checkout_location(
        checkout.to_str().unwrap(),
        &root,
        &parent.join("state"),
        "checkout",
        &nothing(),
    )
    .await
    .unwrap();
    assert_eq!(path, checkout);
    assert_eq!(directory, project);
}

#[tokio::test]
async fn an_excluded_root_refuses_a_workspace_at_its_excluded_directory() {
    let (temp, manager) = crate::test_support::indexed_manager().await;
    let origin = repository(temp.path(), "origin");
    let repo = manager
        .register_repository(format!("file://{}", origin.display()).into(), None, None)
        .await
        .unwrap();
    let home = std::fs::canonicalize(temp.path()).unwrap();
    let placed = home.join(".other.noindex");
    manager
        .create_workspace(&repo.id, "placed".into(), None, None, Some(placed))
        .await
        .unwrap();
    let root = super::ConfiguredRoot {
        path: home.join("other"),
        layout: crate::daemon::workspace::spotlight::Layout::Excluded,
    };
    let repositories = manager.repositories().await.unwrap();
    let workspaces = manager.list_workspaces().await.unwrap();
    let recorded = super::Recorded {
        repositories: &repositories,
        workspaces: &workspaces,
    };
    let error = super::prepare_root(&root, &home.join("state"), &recorded, None)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("inside the workspace placed"),
        "{error:#}"
    );
    assert!(std::fs::symlink_metadata(&root.path).is_err());
}

fn nothing() -> super::Recorded<'static> {
    super::Recorded {
        repositories: &[],
        workspaces: &[],
    }
}

#[tokio::test]
async fn a_checkout_where_the_local_config_belongs_gets_its_own_directory() {
    let (_root, manager) = crate::test_support::manager().await;
    let root = crate::config::Config::default()
        .root_dir(&manager.paths)
        .unwrap();
    std::fs::create_dir_all(root.join("project")).unwrap();
    let checkout = crate::test_support::repository(&root.join("project"), ".shoal");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let local = manager.local_config_path(&repo.id).await.unwrap();
    let checkout = std::fs::canonicalize(&checkout).unwrap();
    assert!(!local.starts_with(&checkout), "{}", local.display());
}
