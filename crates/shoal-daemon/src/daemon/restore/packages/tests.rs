use std::fs;

use crate::{
    daemon::test_support::{commit, git, import_file, manager, repository},
    refusal::Reason,
    removal::{InspectionPolicy, UnsavedWork},
    shoal_dirs::{Contents, ShoalDir},
    transfer::Carrying,
};
use shoal_protocol::space_names::SpaceSelector;

/// A package of a workspace `worker` with a commit of its own, of a
/// repository whose origin is the URL of a bare repository beside it, with
/// the directory both are in and that URL.
async fn exported_worker() -> (tempfile::TempDir, Vec<u8>, String) {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let origin = root.path().join("origin.git");
    let origin = origin.to_str().unwrap();
    git(root.path(), &["init", "-q", "--bare", "-b", "main", origin]);
    let url = format!("file://{origin}");
    git(&path, &["remote", "add", "origin", &url]);
    git(&path, &["push", "-q", "-u", "origin", "main"]);
    git(&path, &["remote", "set-head", "origin", "main"]);
    let repo = (manager.register_repository(path.to_str().unwrap().into(), None, None))
        .await
        .unwrap();
    let worker = (manager.create_workspace(&repo.id, "worker".into(), None, None, None))
        .await
        .unwrap();
    fs::write(worker.path.join("work"), "work\n").unwrap();
    commit(&worker.path, "work");
    let file = root.path().join("worker.shoal");
    let selector = SpaceSelector::id(&worker.id);
    let (all, policy) = (UnsavedWork::All, InspectionPolicy::GitOnly);
    (manager.export_space(&selector, all, policy, file.clone(), &Carrying::Subtree))
        .await
        .unwrap();
    (root, fs::read(file).unwrap(), url)
}

#[tokio::test]
async fn an_import_clones_the_repository_it_lacks_from_its_origin() {
    let (_source, bytes, url) = exported_worker().await;
    let (_target, importing) = manager().await;

    let imported = import_file(&importing, None, &bytes).await.unwrap();

    let [cloned] = importing.repositories().await.unwrap().try_into().unwrap();
    assert_eq!(cloned.source, url);
    let selector = SpaceSelector::id(imported.restored.id());
    let opened = importing.space(&selector).await.unwrap();
    assert_eq!(opened.repository_id, Some(cloned.id));
}

#[tokio::test]
async fn an_import_whose_clone_fails_records_nothing() {
    let (source, bytes, _url) = exported_worker().await;
    fs::remove_dir_all(source.path().join("origin.git")).unwrap();
    let (_target, importing) = manager().await;

    let refused = import_file(&importing, None, &bytes).await.unwrap_err();

    assert_eq!(Reason::of(&refused), Some(Reason::NotImported));
    assert!(importing.repositories().await.unwrap().is_empty());
    assert!(importing.removed_spaces().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_package_cut_short_records_nothing_and_keeps_none_of_it() {
    let (_source, bytes, _url) = exported_worker().await;
    let (_target, importing) = manager().await;

    let short = &bytes[..bytes.len() - 1];
    let refused = import_file(&importing, None, short).await.unwrap_err();

    assert_eq!(Reason::of(&refused), Some(Reason::NotImported));
    assert!(format!("{refused:#}").contains("ends inside a bundle"));
    assert!(importing.removed_spaces().await.unwrap().is_empty());
    let [cloned] = importing.repositories().await.unwrap().try_into().unwrap();
    let imports = ShoalDir::of(&cloned, Contents::Imports);
    let left: Vec<_> = (fs::read_dir(imports.root()).unwrap())
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.to_string_lossy().starts_with("package-"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

/// The manifest of a copy of chat `plan`, whose sessions are newer than its
/// files when `earlier_files`.
fn chat_copy(earlier_files: bool) -> crate::transfer::Manifest {
    serde_json::from_value(serde_json::json!({
        "remote_url": "https://forge.test/o/r",
        "record": {
            "workspace_id": "c1",
            "repository_id": "r1",
            "name": "plan",
            "links": {"issues": [], "prs": [], "opened": [], "related": []},
            "cause": "manual",
            "removed_at": 1,
            "expires_at": 2,
            "chat": {"agent": "claude", "mode": "chat"},
        },
        "copy": {"earlier_files": earlier_files},
    }))
    .unwrap()
}

#[test]
fn a_copy_never_continues_the_original_sessions() {
    let refused = super::copy_warnings(&chat_copy(false), Some(true)).unwrap_err();
    assert!(
        format!("{refused:#}").contains("--keep-sessions"),
        "{refused:#}"
    );
    assert!(super::copy_warnings(&chat_copy(false), Some(false)).is_ok());
    let warned = super::copy_warnings(&chat_copy(true), None).unwrap();
    assert_eq!(warned.len(), 1);
    assert!(
        super::copy_warnings(&chat_copy(false), None)
            .unwrap()
            .is_empty()
    );
}
