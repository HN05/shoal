use std::{fs, path::Path, sync::Arc};

use super::*;
use crate::{
    daemon::workspace::Manager,
    execution::recovery::{self, Record, Session},
    removal::{InspectionPolicy, UnsavedWork},
    test_support::{commit, git, manager, repository},
};

/// A registered repository pushed to a bare `origin`.
async fn exporter() -> (
    tempfile::TempDir,
    Arc<Manager>,
    Repository,
    std::path::PathBuf,
) {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let origin = root.path().join("origin.git");
    git(
        root.path(),
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            origin.to_str().unwrap(),
        ],
    );
    git(
        &path,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&path, &["push", "-q", "-u", "origin", "main"]);
    git(&path, &["remote", "set-head", "origin", "main"]);
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    (root, manager, repo, origin)
}

/// Another Shoal with its own clone of `origin`.
async fn importer(origin: &Path) -> (tempfile::TempDir, Arc<Manager>, Repository) {
    let (root, manager) = manager().await;
    git(
        root.path(),
        &["clone", "-q", origin.to_str().unwrap(), "repo"],
    );
    let repo = manager
        .register_repository(
            root.path().join("repo").to_str().unwrap().into(),
            None,
            None,
        )
        .await
        .unwrap();
    (root, manager, repo)
}

/// Remove `workspace` and export its record.
async fn export_removed(manager: &Manager, repo: &Repository, workspace: &str) -> Vec<u8> {
    manager
        .remove_workspace(workspace, UnsavedWork::All, InspectionPolicy::GitOnly)
        .await
        .unwrap();
    let removed = manager.removed_workspaces().await.unwrap().remove(0);
    let mut file = Vec::new();
    export(repo, removed, &mut file).await.unwrap();
    file
}

async fn import(
    manager: &Manager,
    repo: &Repository,
    file: &[u8],
) -> crate::model::RestoredWorkspace {
    let import = Import::read(&mut &file[..]).unwrap();
    manager
        .import_workspace(
            &repo.id.as_str().into(),
            import.bundle.as_ref().map(|bundle| bundle.path()),
            import.manifest.prerequisite.as_deref(),
            import.manifest.workspace,
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn import_opens_the_exported_branch_with_its_files_and_agents() {
    let (_source, exporting, source_repo, origin) = exporter().await;
    let (_target, importing, target_repo) = importer(&origin).await;
    // The default branch moves after the importer cloned it, so the import
    // fetches what the bundle leaves out.
    fs::write(source_repo.path.join("later"), "later\n").unwrap();
    commit(&source_repo.path, "later");
    git(&source_repo.path, &["push", "-q", "origin", "main"]);
    let workspace = exporting
        .create_workspace(&source_repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    fs::write(workspace.path.join("work"), "work\n").unwrap();
    commit(&workspace.path, "work");
    let head = git(&workspace.path, &["rev-parse", "HEAD"]);
    fs::write(workspace.path.join("scratch"), "notes\n").unwrap();
    let status = git(&workspace.path, &["status", "--porcelain"]);
    recovery::save_record(
        &exporting.paths,
        &workspace.id,
        "stopped-agent",
        "claude",
        Some("overload"),
    )
    .unwrap();

    let file = export_removed(&exporting, &source_repo, &workspace.id).await;
    let read = Import::read(&mut &file[..]).unwrap();
    assert_eq!(read.manifest.remote_url.as_deref(), origin.to_str());
    assert!(read.manifest.bundle);
    let restored = import(&importing, &target_repo, &file).await;

    assert!(restored.warnings.is_empty(), "{:?}", restored.warnings);
    let path = &restored.opened.workspace.path;
    assert!(path.starts_with(target_repo.path.parent().unwrap()));
    assert_eq!(restored.opened.workspace.branch, "topic");
    assert_eq!(git(path, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(path, &["status", "--porcelain"]), status);
    assert_eq!(fs::read_to_string(path.join("scratch")).unwrap(), "notes\n");
    assert!(importing.removed_workspaces().await.unwrap().is_empty());
    let checkpoints = importing
        .checkpoints(&restored.opened.workspace.id, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(checkpoints.len(), 1);
    assert_eq!(checkpoints[0].repository_id, target_repo.id);
    // The stopped agent starts a new session here.
    let saved = recovery::Saved::load(&importing.paths, &restored.opened.workspace.id).unwrap();
    assert_eq!(saved.agents.len(), 1);
    let record = Record::load(&saved.agents[0].1).unwrap().unwrap();
    assert_eq!(record.agent, "claude");
    assert_eq!(record.session(), Session::New);
}

#[tokio::test]
async fn export_leaves_out_a_checkpoint_the_default_branch_has() {
    let (_source, exporting, source_repo, origin) = exporter().await;
    let (_target, importing, target_repo) = importer(&origin).await;
    let workspace = exporting
        .create_workspace(&source_repo.id, "empty".into(), None, None, None)
        .await
        .unwrap();
    let head = git(&workspace.path, &["rev-parse", "HEAD"]);

    let file = export_removed(&exporting, &source_repo, &workspace.id).await;
    let read = Import::read(&mut &file[..]).unwrap();
    assert!(!read.manifest.bundle);
    assert_eq!(read.manifest.prerequisite.as_deref(), Some(head.trim()));
    let restored = import(&importing, &target_repo, &file).await;
    assert_eq!(
        git(&restored.opened.workspace.path, &["rev-parse", "HEAD"]),
        head
    );
}

#[test]
fn import_refuses_other_files_and_newer_formats() {
    let error = Import::read(&mut &b"not an export\n"[..]).err().unwrap();
    assert_eq!(error.to_string(), "not a Shoal workspace export");
    let error = Import::read(&mut &b"shoal workspace 2\n{}\n"[..])
        .err()
        .unwrap();
    assert!(error.to_string().contains("newer format"), "{error}");
}

#[test]
fn moved_records_keep_no_session_place_or_wrapper() {
    let record: Record = serde_json::from_value(serde_json::json!({
        "agent": "codex",
        "session": "s1",
        "place": "tab",
    }))
    .unwrap();
    assert_eq!(record.session(), Session::Id("s1"));
    let moved = record.moved();
    assert_eq!(moved.session(), Session::New);
    assert_eq!((moved.place, moved.waiting), (None, None));
}
