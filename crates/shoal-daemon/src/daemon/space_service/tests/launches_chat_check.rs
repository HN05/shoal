use crate::daemon::space_service::tests::launches_chat::offline_machine;
use crate::daemon::test_support::manager;
use shoal_protocol::launches::{ItemChoice, Launch, LaunchSource, SpaceChoice, StartTarget};
use shoal_protocol::space_names::SpaceSelector;

/// A check of a chat in a workspace's worktree answers with that
/// workspace, and refuses it as the chat's parent, as the start does.
#[tokio::test]
async fn a_chat_check_answers_its_host_and_refuses_it_as_parent() {
    let (temp, manager) = manager().await;
    let repo = offline_machine(&manager, &temp).await;
    let base = Some("main".into());
    let fix = (manager.create_workspace(&repo.id, "fix".into(), base, None, None))
        .await
        .unwrap();
    let hosted = Launch {
        in_workspace: Some(SpaceChoice::Space(SpaceSelector::id(&fix.id))),
        mode: Some("chat".into()),
        ..Launch::default()
    };
    let below_host = Launch {
        parent: Some(SpaceSelector::id(&fix.id)),
        ..hosted.clone()
    };

    let checked = manager
        .spaces
        .check_launch(&*manager, hosted, None)
        .await
        .unwrap();

    assert_eq!(checked.map(|workspace| workspace.id), Some(fix.id));
    let refused = manager
        .spaces
        .check_launch(&*manager, below_host, None)
        .await
        .unwrap_err();
    assert!(
        format!("{refused:#}").contains("cannot go below it"),
        "{refused:#}"
    );
}

/// A check of a plan that asks for its issue leaves it to a picker, which
/// a start without one refuses.
#[tokio::test]
async fn a_chat_check_leaves_a_plan_issue_to_a_picker() {
    let (temp, manager) = manager().await;
    let repo = offline_machine(&manager, &temp).await;
    let launch = Launch {
        target: Some(StartTarget::Repository(repo.id.to_string())),
        mode: Some("plan".into()),
        issue: Some(ItemChoice::Pick),
        agent: Some("claude".into()),
        ..Launch::default()
    };

    let checked = manager
        .spaces
        .check_launch(&*manager, launch.clone(), None)
        .await;

    assert!(checked.unwrap().is_none());
    let started = manager
        .spaces
        .launch(&manager.daemon(), launch, LaunchSource::Manual, None)
        .await;
    assert!(format!("{:#}", started.unwrap_err()).contains("missing issue"));
}
