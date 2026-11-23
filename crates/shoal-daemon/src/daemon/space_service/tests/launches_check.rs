use crate::daemon::{
    manager::Manager,
    test_support::{global_config, manager, repository},
};
use shoal_protocol::launches::{Launch, SpaceChoice, StartTarget};
use shoal_protocol::space_names::SpaceSelector;

/// A registered repository.
async fn registered(manager: &Manager, temp: &tempfile::TempDir) -> crate::model::Repository {
    manager.publish_config(global_config("[agents.claude]\ncommand = ['sh']\n"));
    let path = repository(temp.path(), "repo");
    let path = path.to_str().unwrap().into();
    manager
        .repositories
        .register(path, None, None)
        .await
        .unwrap()
}

/// A check of a launch whose mode a terminal has yet to pick refuses what
/// every kind of space refuses: a repository no one registered and a
/// parent no one knows.
#[tokio::test]
async fn a_check_without_a_mode_refuses_what_every_kind_refuses() {
    let (temp, manager) = manager().await;
    let repo = registered(&manager, &temp).await;
    let unregistered = Launch {
        repository: Some("elsewhere".into()),
        ..Launch::default()
    };
    let orphan = Launch {
        target: Some(StartTarget::Repository(repo.id.to_string())),
        parent: Some("999".parse().unwrap()),
        ..Launch::default()
    };

    assert!(
        manager
            .spaces
            .check_launch(&*manager, unregistered, None)
            .await
            .is_err()
    );
    assert!(
        manager
            .spaces
            .check_launch(&*manager, orphan, None)
            .await
            .is_err()
    );
}

/// A check of a start in an existing workspace whose mode a terminal has
/// yet to pick answers with that workspace, which the start goes into.
#[tokio::test]
async fn a_check_without_a_mode_answers_the_workspace_in_names() {
    let (temp, manager) = manager().await;
    let repo = registered(&manager, &temp).await;
    let base = Some("main".into());
    let fix = (manager.create_workspace(&repo.id, "fix".into(), base, None, None))
        .await
        .unwrap();
    let launch = Launch {
        in_workspace: Some(SpaceChoice::Space(SpaceSelector::id(&fix.id))),
        ..Launch::default()
    };

    let checked = manager
        .spaces
        .check_launch(&*manager, launch, None)
        .await
        .unwrap();

    assert_eq!(checked.map(|workspace| workspace.id), Some(fix.id));
}

/// A check of a start on an issue whose mode a terminal has yet to pick
/// refuses an issue no kind of space could read, before the mode picker.
#[tokio::test]
async fn a_check_without_a_mode_refuses_an_unreadable_issue() {
    let (temp, manager) = manager().await;
    let repo = registered(&manager, &temp).await;
    let launch = Launch {
        target: Some(StartTarget::Repository(repo.id.to_string())),
        issue: Some(shoal_protocol::launches::ItemChoice::Named(
            "34".parse().unwrap(),
        )),
        ..Launch::default()
    };

    assert!(
        manager
            .spaces
            .check_launch(&*manager, launch, None)
            .await
            .is_err()
    );
}
