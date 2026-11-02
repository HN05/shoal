use crate::{
    client::files::Attacher,
    daemon::notifications::NotificationKind,
    model::{SpaceDetail, SpaceStatus},
};

#[tokio::test]
async fn status_counts_only_the_spaces_own_unread_notifications() {
    let (temp, manager) = crate::test_support::manager().await;
    let checkout = crate::test_support::repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    for name in ["own", "other"] {
        manager
            .create_workspace(&repo.id, name.into(), None, None, None)
            .await
            .unwrap();
    }
    let chat = manager
        .start_chat(
            repo.id.clone().into(),
            "planning".into(),
            "claude".into(),
            std::process::id(),
            None,
            false,
        )
        .await
        .unwrap();
    let unread = |status: SpaceStatus| status.unread_notifications;
    let before = unread(manager.space_status("own").await.unwrap());

    for (workspace, message) in [
        (Some("own"), "own"),
        (Some("other"), "other"),
        (None, "daemon"),
    ] {
        manager
            .notify(workspace, NotificationKind::CleanupFailed, message)
            .await;
    }
    manager
        .send_message("planning", "chat".into(), Vec::new(), &Attacher::User)
        .await
        .unwrap();
    let status = manager.space_status("own").await.unwrap();
    assert_eq!(unread(status), before + 1);

    let status = manager.space_status("planning").await.unwrap();
    let SpaceDetail::Chat { chat: shown } = &status.space else {
        panic!("expected a chat: {:?}", status.space);
    };
    assert_eq!(shown.id, chat.id);
    assert_eq!(unread(status), 1);
}
