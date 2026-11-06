use std::{fs, path::Path, sync::Arc};

use crate::{
    daemon::{
        test_support::{commit, git, manager, repository},
        workspace::Manager,
    },
    execution::recovery::{self, Copied, Record},
    model::{RemovedWorkspace, Repository},
    removal::{InspectionPolicy, UnsavedWork},
    transfer::*,
};

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
        .export_space(
            workspace,
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
            file.clone(),
        )
        .await
        .unwrap();
    assert!(exported.removal.is_some());
    // The daemon writes new files only.
    assert!(
        manager
            .export_space(
                workspace,
                UnsavedWork::All,
                InspectionPolicy::GitOnly,
                file.clone(),
            )
            .await
            .is_err()
    );
    fs::read(file).unwrap()
}

async fn import(
    manager: &Manager,
    repo: &Repository,
    file: &[u8],
) -> crate::model::RestoredWorkspace {
    let import = Import::read(&mut &file[..]).unwrap();
    let packaged = packaged_workspace(&import.manifest);
    import_record(manager, repo, &import, packaged.workspace.clone()).await
}

/// Import the workspace `import` packages as `workspace` says, as `shoal
/// move import` does, which keeps the sessions on the exporting machine.
async fn import_here(
    manager: &Manager,
    repo: &Repository,
    file: &[u8],
) -> crate::model::RestoredWorkspace {
    let import = Import::read(&mut &file[..]).unwrap();
    let sessions = import
        .manifest
        .sessions_on(crate::process::machine::id().as_deref(), None);
    let workspace = packaged_workspace(&import.manifest).workspace_with(sessions);
    import_record(manager, repo, &import, workspace).await
}

async fn import_record(
    manager: &Manager,
    repo: &Repository,
    import: &Import,
    workspace: RemovedWorkspace,
) -> crate::model::RestoredWorkspace {
    let packaged = packaged_workspace(&import.manifest);
    manager
        .import_workspace(
            &repo.id.as_str().into(),
            import.bundle.as_ref().map(|bundle| bundle.path()),
            packaged.prerequisite.as_deref(),
            workspace,
            import.manifest.copy.is_some(),
        )
        .await
        .unwrap()
}

/// The workspace `manifest` packages.
fn packaged_workspace(manifest: &Manifest) -> &PackagedWorkspace {
    match &manifest.space {
        Space::Workspace(packaged) => packaged,
        Space::Chat(_) => panic!("a chat package: {manifest:?}"),
    }
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
        &crate::execution::TrackedAgent::named("claude"),
        Some("overload"),
        None,
    )
    .unwrap();

    let file = export_removed(&exporting, &workspace.id).await;
    let read = Import::read(&mut &file[..]).unwrap();
    assert_eq!(read.manifest.remote_url.as_deref(), origin.to_str());
    assert!(packaged_workspace(&read.manifest).bundle);
    // The exporting machine's import can resume the session it recorded.
    let sessions = read.manifest.sessions.as_ref().unwrap();
    assert_eq!(sessions.machine, crate::process::machine::id());
    assert_eq!(sessions.agents[0].record.session.as_deref(), Some("s1"));
    let restored = import(&importing, &target_repo, &file).await;

    assert!(restored.warnings.is_empty(), "{:?}", restored.warnings);
    let path = &restored.opened.workspace.path;
    assert!(path.starts_with(target_repo.path.parent().unwrap()));
    assert_eq!(restored.opened.workspace.branch.as_deref(), Some("topic"));
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
    // The stopped agent starts a new session on another machine.
    let saved = recovery::Saved::load(&importing.paths, &restored.opened.workspace.id).unwrap();
    assert_eq!(saved.agents.len(), 1);
    let record = Record::load(&saved.agents[0].1).unwrap().unwrap();
    assert_eq!(record.agent, "claude");
    assert!(record.moved);
    assert_eq!(record.session, None);
}

#[tokio::test]
async fn import_writes_nothing_while_the_repository_moves() {
    let (_source, exporting, source_repo, origin) = exporter().await;
    let (_target, importing, target_repo) = importer(&origin).await;
    let workspace = exporting
        .create_workspace(&source_repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    fs::write(workspace.path.join("work"), "work\n").unwrap();
    commit(&workspace.path, "work");
    let file = export_removed(&exporting, &workspace.id).await;

    // A move that has not finished: its checkout may be half copied.
    let id = target_repo.id.clone();
    importing
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO repository_moves
                    (repository_id,source,destination,source_id,destination_id,states)
                    VALUES (?1,'/old','/new','old','new','{}')",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let import = Import::read(&mut &file[..]).unwrap();
    let packaged = packaged_workspace(&import.manifest);
    let imported = importing
        .import_workspace(
            &target_repo.id.as_str().into(),
            import.bundle.as_ref().map(|bundle| bundle.path()),
            packaged.prerequisite.as_deref(),
            packaged.workspace.clone(),
            false,
        )
        .await;
    assert!(imported.is_err());
    assert!(importing.removed_workspaces().await.unwrap().is_empty());
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
    let packaged = packaged_workspace(&read.manifest);
    assert!(!packaged.bundle);
    assert_eq!(packaged.prerequisite.as_deref(), Some(head.trim()));
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

    let exported = exporting
        .export_space(
            &workspace.id,
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
            blocked.join("workspace.shoal"),
        )
        .await;
    assert!(exported.is_err());
    assert!(exporting.workspace(&workspace.id).await.is_ok());
    assert!(workspace.path.exists());
}

#[tokio::test]
async fn a_copy_keeps_the_workspace_and_imports_on_a_branch_of_its_own() {
    use crate::{
        forge::items::link::LinkKind,
        model::{LinkedItem, Relation},
    };
    let (_source, manager, repo, _origin) = exporter().await;
    let workspace = manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    fs::write(workspace.path.join("work"), "work\n").unwrap();
    commit(&workspace.path, "work");
    let head = git(&workspace.path, &["rev-parse", "HEAD"]);
    fs::write(workspace.path.join("scratch"), "notes\n").unwrap();
    let item = |kind, url: &str| LinkedItem {
        kind,
        url: url.into(),
        title: None,
    };
    let pr = item(LinkKind::Pr, "https://forge.test/o/r/pulls/2");
    let issue = item(LinkKind::Issue, "https://forge.test/o/r/issues/3");
    manager
        .record_items(
            &workspace.id,
            vec![(Relation::Opened, pr), (Relation::Related, issue.clone())],
        )
        .await
        .unwrap();
    recovery::save_session(&manager.paths, &workspace.id, "stopped-agent", "s1").unwrap();
    recovery::save_record(
        &manager.paths,
        &workspace.id,
        "stopped-agent",
        &crate::execution::TrackedAgent::named("claude"),
        None,
        None,
    )
    .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("topic.shoal");

    let exported = manager
        .copy_space(&workspace.name, None, file.clone())
        .await
        .unwrap();
    assert!(exported.removal.is_none());
    assert_eq!(exported.manifest.copy, Some(Copied::default()));
    let kept = manager.workspace(&workspace.id).await.unwrap();
    assert_eq!(kept.path, workspace.path);
    assert_eq!(
        recovery::Saved::load(&manager.paths, &workspace.id)
            .unwrap()
            .agents
            .len(),
        1
    );
    let bytes = fs::read(&file).unwrap();
    assert!(bytes.starts_with(format!("{FORMAT} 5").as_bytes()));
    let restored = import_here(&manager, &repo, &bytes).await;

    let copy = &restored.opened.workspace;
    assert_ne!(copy.id, workspace.id);
    assert_eq!(copy.branch.as_deref(), Some("topic-2"));
    assert_eq!(git(&copy.path, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        fs::read_to_string(copy.path.join("scratch")).unwrap(),
        "notes\n"
    );
    let links = manager.workspace(&copy.id).await.unwrap().links;
    assert!(links.opened.is_empty(), "{links:?}");
    assert_eq!(links.related, [issue]);
    // Its agent starts from a fork of the session it reported here, which
    // only a machine with an ID knows it exported; without one, nor any of
    // the session's files, it starts anew.
    let saved = recovery::Saved::load(&manager.paths, &copy.id).unwrap();
    let record = Record::load(&saved.agents[0].1).unwrap().unwrap();
    let forked = crate::process::machine::id().map(|_| "s1");
    assert_eq!(record.session.as_deref(), forked);
    assert_eq!(record.copied, Some(Copied::default()));
    assert_eq!(record.moved, forked.is_none());
    // The original keeps its branch, files and agent.
    assert_eq!(git(&workspace.path, &["rev-parse", "HEAD"]), head);
    assert!(workspace.path.join("scratch").exists());
}

#[tokio::test]
async fn a_copy_of_an_earlier_checkpoint_marks_its_sessions_newer() {
    let (_source, manager, repo, _origin) = exporter().await;
    let workspace = manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    let first = manager.take_checkpoint(&workspace.id, None).await.unwrap();
    fs::write(workspace.path.join("later"), "later\n").unwrap();
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("topic.shoal");

    let exported = manager
        .copy_space(&workspace.name, Some(first.number), file.clone())
        .await
        .unwrap();
    assert_eq!(
        exported.manifest.copy,
        Some(Copied {
            earlier_files: true
        })
    );
    let restored = import(&manager, &repo, &fs::read(&file).unwrap()).await;
    assert!(!restored.opened.workspace.path.join("later").exists());
    assert!(
        manager
            .copy_space(
                &workspace.name,
                Some(99),
                directory.path().join("missing.shoal")
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_removed_workspace_has_nothing_to_copy() {
    let (_source, manager, repo, _origin) = exporter().await;
    let workspace = manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    manager
        .remove_workspace(&workspace.id, UnsavedWork::All, InspectionPolicy::GitOnly)
        .await
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let refused = manager
        .copy_space("topic", None, directory.path().join("topic.shoal"))
        .await
        .unwrap_err();
    assert!(
        format!("{refused:#}").contains("was removed"),
        "{refused:#}"
    );
}

#[tokio::test]
async fn concurrent_imports_of_a_copy_take_branches_of_their_own() {
    let (_source, manager, repo, _origin) = exporter().await;
    let workspace = manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("topic.shoal");
    manager
        .copy_space(&workspace.name, None, file.clone())
        .await
        .unwrap();
    let bytes = fs::read(&file).unwrap();

    let (first, second) = tokio::join!(
        import_here(&manager, &repo, &bytes),
        import_here(&manager, &repo, &bytes)
    );
    let mut branches = [first, second].map(|restored| restored.opened.workspace.branch.unwrap());
    branches.sort();
    assert_eq!(branches, ["topic-2", "topic-3"]);
}
