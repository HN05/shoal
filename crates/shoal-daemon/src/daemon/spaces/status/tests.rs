use crate::daemon::notifications::AlertKind;
use shoal_protocol::status::{SpaceDetail, SpaceStatus};

#[tokio::test]
async fn status_counts_only_the_spaces_own_unread_notifications() {
    let (temp, manager) = crate::daemon::test_support::manager().await;
    let checkout = crate::daemon::test_support::repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
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

    let kind = AlertKind::CleanupFailed;
    for workspace in &workspaces {
        let message = workspace.name.to_string();
        (manager.coordination.notify_space(
            &*manager,
            &workspace.name,
            &workspace.id,
            kind,
            message,
        ))
        .await;
    }
    manager
        .coordination
        .notify(&*manager, None, kind, "daemon")
        .await;
    manager
        .coordination
        .send_alert(
            &*manager,
            Some(&crate::daemon::test_support::caller_in(
                &manager.named("planning").await,
            )),
            crate::protocol::AlertTarget::Up,
            "chat".into(),
            Vec::new(),
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
async fn status_counts_the_threads_the_space_opened_that_wait() {
    let (temp, manager) = crate::daemon::test_support::manager().await;
    let checkout = crate::daemon::test_support::repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
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
    let planning = manager
        .space(&manager.named("planning").await)
        .await
        .unwrap();
    crate::daemon::test_support::ask(&manager, &planning.id, "Which database?").await;

    let waiting = |status: SpaceStatus| status.attention.waiting_threads;
    assert_eq!(
        waiting(
            manager
                .space_status(&manager.named("planning").await)
                .await
                .unwrap()
        ),
        1
    );
    assert_eq!(
        waiting(
            manager
                .space_status(&manager.named("own").await)
                .await
                .unwrap()
        ),
        0
    );
}

#[tokio::test]
async fn a_chats_status_shows_its_agents_retries() {
    use crate::{state::TurnFailure, time::UnixTime};
    use shoal_protocol::executions::ProxyRetry;
    let (_temp, manager) = crate::daemon::test_support::manager().await;
    let chat = crate::daemon::test_support::chat(&manager, "talk", "chat", None, None).await;
    let execution = manager.workspace_executions(&chat.id).await.unwrap()[0]
        .id
        .clone();
    let selector = shoal_protocol::space_names::SpaceSelector::id(&chat.id);
    let retry = ProxyRetry {
        failure: TurnFailure::Overloaded,
        attempt: 2,
        next_at: UnixTime::from_seconds(1_000),
    };
    manager
        .agents
        .report_proxy_retry(&*manager, &selector, execution.clone(), Some(retry.clone()))
        .await
        .unwrap();
    let failure = TurnFailure::ServerError;
    manager
        .agents
        .report_turn_failure(
            &*manager,
            &selector,
            failure,
            Some(execution),
            None,
            Default::default(),
            None,
        )
        .await
        .unwrap();
    let status = manager.space_status(&selector).await.unwrap();
    assert_eq!(status.proxy_retry, Some(retry));
    let turn_retry = status
        .turn_retry
        .map(|retry| (retry.failure, retry.attempt));
    assert_eq!(turn_retry, Some((failure, 1)));
}
