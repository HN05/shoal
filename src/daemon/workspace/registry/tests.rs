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
    let error = super::prepare_root(&root, &state, &[], None)
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
    let error = super::prepare_root(&root, &parent.join("state"), &[recorded], None)
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
        &[],
    )
    .await
    .unwrap();
    assert_eq!(path, checkout);
    assert_eq!(directory, project);
}
