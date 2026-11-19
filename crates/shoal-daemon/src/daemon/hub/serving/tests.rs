use std::time::Duration;

use shoal_protocol::hub::routing::{Forwarded, Vouched, VouchedSpace};

use crate::daemon::{
    hub::streams::tests::linked,
    test_daemon::{HANG, TestDaemon},
    workspace::ExecutionKind,
};

#[tokio::test]
async fn a_streamed_request_s_vouch_stands_for_its_caller_only_until_its_answer_starts() {
    let daemon = TestDaemon::start().await;
    let manager = &daemon.manager;
    let workspace = daemon.workspace("followed").await;
    // Its agent reported a session, whose files this machine has none of
    // yet, so the follow stays open.
    let execution = manager
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
        .await
        .unwrap()
        .plan
        .id;
    let agent = crate::execution::TrackedAgent::named("claude");
    (manager.track_agent(
        &execution,
        &agent,
        &workspace.id,
        &workspace.name,
        true,
        Duration::ZERO,
    ))
    .await;
    crate::execution::recovery::save_session(
        &manager.paths,
        &workspace.id,
        &execution,
        &"followed".into(),
    )
    .unwrap();
    let repository = Some(&daemon.repository.id);
    let caller = Vouched::Space(VouchedSpace {
        space: workspace.id.clone(),
        everywhere: false,
        permissions: manager
            .space_permissions(&workspace.id, repository)
            .await
            .unwrap(),
        granted: None,
        read_only: false,
        priority: manager.space_priority(&workspace.id).await.unwrap(),
        chat_modes: Vec::new(),
    });
    let request = Forwarded {
        caller,
        method: serde_json::json!({"follow_session": {"space": workspace.id}}),
        summary: "follow".into(),
    };
    let link = linked();
    let (mut incoming, out) = link.stream();
    manager.serve_streamed(request, out).unwrap();
    let started = tokio::time::timeout(HANG, incoming.next()).await.unwrap();
    let started = String::from_utf8(started.unwrap().unwrap()).unwrap();
    assert!(started.contains("followed_session"), "{started}");
    // The follow goes on without the vouch, which later requests of the
    // space's process bring afresh.
    assert!(manager.vouched_space(&workspace.id).is_none());
}

#[tokio::test]
async fn a_request_whose_answer_goes_on_is_refused_where_answers_are_collected() {
    let daemon = TestDaemon::start().await;
    let workspace = daemon.workspace("followed").await;
    let request = Forwarded {
        caller: Vouched::User,
        method: serde_json::json!({"follow_session": {"space": workspace.id}}),
        summary: "follow".into(),
    };
    let refused = daemon.manager.serve_forwarded(request).await.unwrap_err();
    assert!(refused.to_string().contains("stream"), "{refused:#}");
}
