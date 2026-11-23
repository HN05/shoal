use std::{fs, path::Path};

use crate::{
    daemon::manager::Manager,
    daemon::{
        scope::Caller,
        test_support::{self, commit, git, import_file, import_file_as, manager, repository},
    },
    model::Repository,
    refusal::Reason,
    removal::UnsavedWork,
    shoal_dirs::{Contents, ShoalDir},
    transfer::Carrying,
};
use shoal_protocol::space_names::SpaceSelector;

/// A package of a workspace `worker` with a commit of its own, of a
/// repository whose origin is the URL of a bare repository beside it, with
/// the directory both are in and that URL.
async fn exported_worker() -> (tempfile::TempDir, Vec<u8>, String) {
    exported_worker_with(|origin| format!("file://{origin}")).await
}

/// [`exported_worker`] with the origin `named` names by the bare
/// repository's path.
async fn exported_worker_with(
    named: impl FnOnce(&str) -> String,
) -> (tempfile::TempDir, Vec<u8>, String) {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let origin = root.path().join("origin.git");
    let origin = origin.to_str().unwrap();
    git(root.path(), &["init", "-q", "--bare", "-b", "main", origin]);
    let url = named(origin);
    git(&path, &["remote", "add", "origin", &url]);
    git(&path, &["push", "-q", "-u", "origin", "main"]);
    git(&path, &["remote", "set-head", "origin", "main"]);
    let repo = (manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None))
    .await
    .unwrap();
    let worker = (manager.create_workspace(&repo.id, "worker".into(), None, None, None))
        .await
        .unwrap();
    fs::write(worker.path.join("work"), "work\n").unwrap();
    commit(&worker.path, "work");
    let file = root.path().join("worker.shoal");
    let selector = SpaceSelector::id(&worker.id);
    let all = UnsavedWork::All;
    let moving = crate::daemon::test_support::move_to_file(
        &manager,
        &selector,
        all,
        file.clone(),
        &Carrying::Subtree,
    );
    moving.await.unwrap();
    (root, fs::read(file).unwrap(), url)
}

/// A repository `name` in `root` whose origin is the URL of a bare
/// repository beside it, registered with `manager`; that URL.
async fn with_origin(manager: &Manager, root: &Path, name: &str) -> (Repository, String) {
    let path = repository(root, name);
    let origin = root.join(format!("{name}.git"));
    let origin = origin.to_str().unwrap();
    git(root, &["init", "-q", "--bare", "-b", "main", origin]);
    let url = format!("file://{origin}");
    git(&path, &["remote", "add", "origin", &url]);
    git(&path, &["push", "-q", "-u", "origin", "main"]);
    git(&path, &["remote", "set-head", "origin", "main"]);
    let registered = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None);
    (registered.await.unwrap(), url)
}

#[tokio::test]
async fn an_import_clones_the_repository_it_lacks_from_its_origin() {
    let (_source, bytes, url) = exported_worker().await;
    let (_target, importing) = manager().await;

    let imported = import_file(&importing, None, &bytes).await.unwrap();

    let [cloned] = importing
        .repositories
        .list()
        .await
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(cloned.source, url);
    let selector = SpaceSelector::id(imported.restored.id());
    let opened = importing.spaces.space(&selector).await.unwrap();
    assert_eq!(opened.repository_id, Some(cloned.id));
}

#[tokio::test]
async fn an_import_clones_an_origin_named_by_its_path() {
    let (_source, bytes, path) = exported_worker_with(str::to_owned).await;
    let (_target, importing) = manager().await;

    import_file(&importing, None, &bytes).await.unwrap();

    let [cloned] = importing
        .repositories
        .list()
        .await
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(cloned.source, format!("file://{path}"));
    assert_ne!(cloned.path, path);
}

#[tokio::test]
async fn an_import_whose_clone_fails_records_nothing() {
    let (source, bytes, _url) = exported_worker().await;
    fs::remove_dir_all(source.path().join("origin.git")).unwrap();
    let (_target, importing) = manager().await;

    let refused = import_file(&importing, None, &bytes).await.unwrap_err();

    assert_eq!(Reason::of(&refused), Some(Reason::NotImported));
    assert!(importing.repositories.list().await.unwrap().is_empty());
    assert!(importing.spaces.removed_spaces().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_move_through_the_hub_whose_claim_the_hub_cannot_take_records_nothing() {
    let (_source, bytes, _url) = exported_worker().await;
    let (target, importing) = manager().await;
    let file = target.path().join("moved.shoal");
    fs::write(&file, &bytes).unwrap();
    let import = crate::transfer::PackageImport {
        package: crate::transfer::PackageSource::File(file.clone()),
        repository: None,
        sessions: None,
        claim: crate::transfer::Claim::Move(shoal_ids::move_tickets::MoveTicket::generate()),
    };

    // A machine in no hub has none to claim the space at.
    let input = crate::daemon::restore::packages::PackageInput::File(file);
    let refused = (importing
        .spaces
        .import_package(&*importing, &import, input, None)
        .await)
        .unwrap_err();

    assert_eq!(Reason::of(&refused), Some(Reason::NotImported));
    assert!(importing.spaces.removed_spaces().await.unwrap().is_empty());
    assert!(
        importing
            .spaces
            .spaces_within(&*importing, None)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_package_cut_short_records_nothing_and_keeps_none_of_it() {
    let (_source, bytes, _url) = exported_worker().await;
    let (_target, importing) = manager().await;

    let short = &bytes[..bytes.len() - 1];
    let refused = import_file(&importing, None, short).await.unwrap_err();

    assert_eq!(Reason::of(&refused), Some(Reason::NotImported));
    assert!(format!("{refused:#}").contains("ends inside a bundle"));
    assert!(importing.spaces.removed_spaces().await.unwrap().is_empty());
    let [cloned] = importing
        .repositories
        .list()
        .await
        .unwrap()
        .try_into()
        .unwrap();
    let imports = ShoalDir::of(&cloned, Contents::Imports);
    let left: Vec<_> = (fs::read_dir(imports.root()).unwrap())
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.to_string_lossy().starts_with("package-"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[tokio::test]
async fn a_scoped_import_records_no_space_outside_its_own_repository() {
    let (source, exporting) = manager().await;
    let (other, url) = with_origin(&exporting, source.path(), "other").await;
    let lead = test_support::chat(&exporting, "lead", "chat", None, None).await;
    test_support::working_below(&exporting, &other, "worker", &lead.id, "worker").await;
    let file = source.path().join("lead.shoal");
    let selector = SpaceSelector::id(&lead.id);
    let all = UnsavedWork::All;
    let moving = crate::daemon::test_support::move_to_file(
        &exporting,
        &selector,
        all,
        file.clone(),
        &Carrying::Subtree,
    );
    moving.await.unwrap();
    let (target, importing) = manager().await;
    let own = repository(target.path(), "own");
    let own = (importing
        .repositories
        .register(own.to_str().unwrap().into(), None, None))
    .await
    .unwrap();
    git(target.path(), &["clone", "-q", &url, "other"]);
    let clone = target.path().join("other");
    (importing
        .repositories
        .register(clone.to_str().unwrap().into(), None, None))
    .await
    .unwrap();
    let here = (importing.create_workspace(&own.id, "here".into(), None, None, None))
        .await
        .unwrap();
    let caller = Caller::of_space(here.id);

    let bytes = fs::read(file).unwrap();
    let refused = (import_file_as(&importing, None, &bytes, Some(&caller)).await).unwrap_err();

    assert_eq!(Reason::of(&refused), Some(Reason::NotImported));
    assert!(
        format!("{refused:#}").contains("own repository"),
        "{refused:#}"
    );
    assert!(importing.spaces.removed_spaces().await.unwrap().is_empty());
    assert!(
        importing
            .spaces
            .find_space(&lead.id)
            .await
            .unwrap()
            .is_none()
    );
}
