//! The hub's packages of an intermittent machine's spaces: a space keeps
//! running while its package is written, and another machine continues it
//! from the package under its ID; the machine then retires its copy.
use super::transfer::{exporter, importer};
use crate::{
    daemon::{restore::PackageInput, test_support},
    model::Space,
    transfer::{PackageImport, PackageSource},
};
use shoal_ids::space_ids::SpaceId;

#[tokio::test]
async fn another_machine_continues_a_kept_package_under_its_id() {
    let (_source, exporting, source_repo, origin) = exporter().await;
    let (_target, importing, _) = importer(&origin).await;
    let plan = test_support::chat(&exporting, "plan", "chat", Some(&source_repo), None).await;
    let space = space(&exporting, &plan.id).await;
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("plan.shoal");
    exporting
        .write_kept_package(&space, file.clone())
        .await
        .unwrap();
    // It keeps running here meanwhile.
    assert!(exporting.find_space(&plan.id).await.unwrap().is_some());
    let import = PackageImport {
        package: PackageSource::File(file.clone()),
        repository: None,
        sessions: None,
        ticket: None,
    };
    let input = PackageInput::File(file);
    let imported = importing
        .import_package(&import, input, None)
        .await
        .unwrap();
    assert_eq!(imported.restored.id(), &plan.id);
}

#[tokio::test]
async fn a_retired_chat_keeps_its_agent_s_session_in_its_removal_record() {
    let (_source, manager, repo, _) = exporter().await;
    let plan = test_support::chat(&manager, "plan", "chat", Some(&repo), None).await;
    test_support::report_session(&manager, &plan.id, "plan-session").await;
    let space = space(&manager, &plan.id).await;
    let opens = manager.retire_space(&space).await.unwrap();
    assert!(manager.find_space(&plan.id).await.unwrap().is_none());
    let removed = (manager.removed_spaces().await.unwrap().into_iter())
        .find(|removed| removed.space_id == plan.id)
        .unwrap();
    assert!(!removed.saved_no_session());
    // A chat is no copy's source, so what opens its work is its session.
    assert!(opens.contains("plan-session"), "{opens}");
}

#[tokio::test]
async fn a_retired_workspace_keeps_a_checkpoint_and_leaves_the_spaces_below_it() {
    let (_source, manager, repo, _) = exporter().await;
    let fix = manager
        .create_workspace(repo.id.clone(), "fix".into(), None, None, None)
        .await
        .unwrap();
    let below = test_support::chat(&manager, "below", "chat", Some(&repo), Some(&fix.id)).await;
    let space = space(&manager, &fix.id).await;
    let opens = manager.retire_space(&space).await.unwrap();
    let removed = (manager.removed_spaces().await.unwrap().into_iter())
        .find(|removed| removed.space_id == fix.id)
        .unwrap();
    let number = removed.workspace().unwrap().checkpoint.number;
    // Its ID names the space on the machine that took it over, so a copy
    // of the work kept here names its checkpoint.
    let copy = format!("--copy {} --checkpoint {number}", fix.id);
    assert!(opens.contains(&copy), "{opens}");
    assert!(manager.find_space(&below.id).await.unwrap().is_some());
}

/// The space `id` here.
async fn space(manager: &crate::daemon::manager::Manager, id: &SpaceId) -> Space {
    manager.find_space(id).await.unwrap().unwrap()
}
