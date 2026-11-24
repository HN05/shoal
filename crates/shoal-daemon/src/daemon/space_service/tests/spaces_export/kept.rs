//! The hub's packages of an intermittent machine's spaces: a space keeps
//! running while its package is written, and another machine continues it
//! from the package under its ID; the machine then retires its copy.
use super::transfer::{exporter, importer};
use crate::reviews::ReviewTarget;
use crate::{
    daemon::{restore::PackageInput, test_support},
    model::Space,
    transfer::{PackageImport, PackageSource},
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

#[tokio::test]
async fn another_machine_continues_a_kept_package_under_its_id() {
    let (_source, exporting, source_repo, origin) = exporter().await;
    let (_target, importing, _) = importer(&origin).await;
    let plan = test_support::chat(&exporting, "plan", "chat", Some(&source_repo), None).await;
    let space = space(&exporting, &plan.id).await;
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("plan.shoal");
    exporting
        .spaces
        .write_kept_package(&*exporting, &space, file.clone())
        .await
        .unwrap();
    // It keeps running here meanwhile.
    assert!(
        exporting
            .spaces
            .find_space(&plan.id)
            .await
            .unwrap()
            .is_some()
    );
    let import = PackageImport {
        package: PackageSource::File(file.clone()),
        repository: None,
        sessions: None,
        claim: crate::transfer::Claim::Plain,
    };
    let input = PackageInput::File(file);
    let imported = importing
        .spaces
        .import_package(&*importing, &import, input, None)
        .await
        .unwrap();
    assert_eq!(imported.restored.id(), &plan.id);
}

/// A workspace's queued review goes with its package and is queued again
/// where the package is imported.
#[tokio::test]
async fn a_kept_package_carries_a_workspace_s_queued_review() {
    let (_source, exporting, repo, origin) = exporter().await;
    let (_target, importing, _) = importer(&origin).await;
    let reviewer =
        "[review]\nagent = 'fake'\n[agents.fake]\ncommand = ['true']\nreview = ['{prompt}']\n";
    test_support::configure(&exporting, reviewer).await;
    test_support::configure(&importing, reviewer).await;
    let fix = exporting
        .create_workspace(repo.id.clone(), "fix".into(), None, None, None)
        .await
        .unwrap();
    let model = crate::agent::ModelChoice {
        model: Some("opus".into()),
        effort: None,
    };
    (exporting.reviews)
        .queue(
            exporting.as_ref(),
            &SpaceSelector::id(&fix.id),
            ReviewTarget::Workspace,
            None,
            model.clone(),
        )
        .await
        .unwrap();
    let space = space(&exporting, &fix.id).await;
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("fix.shoal");
    (exporting
        .spaces
        .write_kept_package(&*exporting, &space, file.clone()))
    .await
    .unwrap();
    let import = PackageImport {
        package: PackageSource::File(file.clone()),
        repository: None,
        sessions: None,
        claim: crate::transfer::Claim::Plain,
    };
    (importing
        .spaces
        .import_package(&*importing, &import, PackageInput::File(file), None))
    .await
    .unwrap();
    let queued = importing.reviews.pending(&fix.id).await.unwrap().unwrap();
    assert_eq!((queued.agent.name(), queued.model), ("fake", model));
}

#[tokio::test]
async fn a_retired_chat_keeps_its_agent_s_session_in_its_removal_record() {
    let (_source, manager, repo, _) = exporter().await;
    let plan = test_support::chat(&manager, "plan", "chat", Some(&repo), None).await;
    test_support::report_session(&manager, &plan.id, "plan-session").await;
    let space = space(&manager, &plan.id).await;
    let opens = manager
        .spaces
        .retire_space(&*manager, &space)
        .await
        .unwrap();
    assert!(manager.spaces.find_space(&plan.id).await.unwrap().is_none());
    let removed = (manager.spaces.removed_spaces().await.unwrap().into_iter())
        .find(|removed| removed.space_id == plan.id)
        .unwrap();
    assert!(!removed.saved_no_session());
    // A chat is no copy's source, so what opens its work is its session.
    assert!(opens.contains("plan-session"), "{opens}");
}

/// The space `id` here.
async fn space(manager: &crate::daemon::manager::Manager, id: &SpaceId) -> Space {
    manager.spaces.find_space(id).await.unwrap().unwrap()
}
