use std::{fs, path::Path, sync::Arc};

use super::*;
use crate::{
    daemon::workspace::Manager,
    execution::recovery::{self, Record},
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

/// Export `workspace`, removing it, and read the file back.
async fn export_removed(manager: &Manager, workspace: &str) -> Vec<u8> {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("workspace.shoal");
    let exported = manager
        .export_workspace(
            workspace,
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
            file.clone(),
        )
        .await
        .unwrap();
    assert!(exported.removal.is_some());
    // The daemon writes new files only.
    let error = manager
        .export_workspace(
            workspace,
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
            file.clone(),
        )
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("write"), "{error:#}");
    fs::read(file).unwrap()
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
    recovery::save_session(&exporting.paths, &workspace.id, "stopped-agent", "s1").unwrap();
    recovery::save_record(
        &exporting.paths,
        &workspace.id,
        "stopped-agent",
        "claude",
        Some("overload"),
    )
    .unwrap();

    let file = export_removed(&exporting, &workspace.id).await;
    let read = Import::read(&mut &file[..]).unwrap();
    assert_eq!(read.manifest.remote_url.as_deref(), origin.to_str());
    assert!(read.manifest.bundle);
    // The exporting machine's import can resume the session it recorded.
    let sessions = read.manifest.sessions.as_ref();
    assert_eq!(
        sessions.map(|sessions| sessions.machine.clone()),
        crate::process::machine::id()
    );
    if let Some(sessions) = sessions {
        assert_eq!(sessions.agents[0].record.session.as_deref(), Some("s1"));
    }
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
    assert!(record.moved);
    assert_eq!(record.session, None);
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

    let file = export_removed(&exporting, &workspace.id).await;
    let read = Import::read(&mut &file[..]).unwrap();
    assert!(!read.manifest.bundle);
    assert_eq!(read.manifest.prerequisite.as_deref(), Some(head.trim()));
    let restored = import(&importing, &target_repo, &file).await;
    assert_eq!(
        git(&restored.opened.workspace.path, &["rev-parse", "HEAD"]),
        head
    );
}

#[tokio::test]
async fn export_the_daemon_cannot_write_keeps_the_workspace() {
    let (source, exporting, source_repo, _origin) = exporter().await;
    let workspace = exporting
        .create_workspace(&source_repo.id, "kept".into(), None, None, None)
        .await
        .unwrap();
    let blocked = source.path().join("file");
    fs::write(&blocked, "").unwrap();

    let error = exporting
        .export_workspace(
            &workspace.id,
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
            blocked.join("workspace.shoal"),
        )
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("the Shoal daemon cannot write to"),
        "{error:#}"
    );
    assert!(exporting.workspace(&workspace.id).await.is_ok());
    assert!(workspace.path.exists());
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
    assert!(!record.moved);
    let moved = record.moved();
    assert!(moved.moved);
    assert_eq!(
        (moved.session, moved.place, moved.waiting),
        (None, None, None)
    );
}
