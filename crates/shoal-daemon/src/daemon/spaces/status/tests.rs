use crate::{
    client::files::Attacher,
    daemon::notifications::NotificationKind,
    model::{SpaceDetail, SpaceStatus},
};

#[tokio::test]
async fn status_counts_only_the_spaces_own_unread_notifications() {
    let (temp, manager) = crate::daemon::test_support::manager().await;
    let checkout = crate::daemon::test_support::repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let mut workspaces = Vec::new();
    for name in ["own", "other"] {
        let workspace = manager
            .create_workspace(&repo.id, name.into(), None, None, None)
            .await
            .unwrap();
        workspaces.push(workspace);
    }
    let chat = manager
        .start_chat(
            repo.id.clone().into(),
            "planning".into(),
            "claude".into(),
            None,
        )
        .await
        .unwrap();
    let unread = |status: SpaceStatus| status.attention.unread_notifications;
    let before = unread(
        manager
            .space_status(&manager.named("own").await)
            .await
            .unwrap(),
    );

    let kind = NotificationKind::CleanupFailed;
    for workspace in &workspaces {
        let message = workspace.name.to_string();
        (manager.notify_space(&workspace.name, &workspace.id, kind, message)).await;
    }
    manager.notify(None, kind, "daemon").await;
    manager
        .send_message(
            &manager.named("planning").await,
            "chat".into(),
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    let status = manager
        .space_status(&manager.named("own").await)
        .await
        .unwrap();
    assert_eq!(unread(status), before + 1);

    let status = manager
        .space_status(&manager.named("planning").await)
        .await
        .unwrap();
    let SpaceDetail::Chat { chat: shown } = &status.space else {
        panic!("expected a chat: {:?}", status.space);
    };
    assert_eq!(shown.id, chat.id);
    assert_eq!(unread(status), 1);
}

#[tokio::test]
async fn status_counts_the_spaces_own_open_questions() {
    let (temp, manager) = crate::daemon::test_support::manager().await;
    let checkout = crate::daemon::test_support::repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager
        .create_workspace(&repo.id, "own".into(), None, None, None)
        .await
        .unwrap();
    manager
        .start_chat(
            repo.id.clone().into(),
            "planning".into(),
            "claude".into(),
            None,
        )
        .await
        .unwrap();
    let ask = crate::client::questions::Ask {
        question: "Use SQLite?".into(),
        recommendation: "yes".into(),
        reason: None,
        choices: Vec::new(),
    };
    manager
        .ask_question(
            &manager.named("planning").await,
            ask,
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();

    let questions = |status: SpaceStatus| status.attention.open_questions;
    assert_eq!(
        questions(
            manager
                .space_status(&manager.named("planning").await)
                .await
                .unwrap()
        ),
        1
    );
    assert_eq!(
        questions(
            manager
                .space_status(&manager.named("own").await)
                .await
                .unwrap()
        ),
        0
    );
}
