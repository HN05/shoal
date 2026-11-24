use crate::scope::Caller;
use crate::stand_in::Fixture;
use shoal_protocol::protocol::AlertTarget;

#[tokio::test]
async fn only_the_agent_of_a_space_marks_its_turn() {
    let manager = Fixture::new().await;
    let repo = manager.repository("repo").await;
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    manager.run_agent(&parent.id).await;
    let user = manager
        .spaces
        .mark_chat_alert(&*manager, None, AlertTarget::Up, Vec::new());
    assert!(user.await.is_err());
    // A process of the space that runs no agent.
    let command = Caller::of_space(parent.id.clone());
    let marked =
        (manager.spaces).mark_chat_alert(&*manager, Some(&command), AlertTarget::Up, Vec::new());
    assert!(marked.await.is_err());
}

#[tokio::test]
async fn a_coordinator_posts_in_its_chat_only_in_threads_it_takes_part_in() {
    use shoal_daemon_base::callers::ScopedExecution;
    use shoal_protocol::space_names::SpaceSelector;
    use shoal_protocol::threads::{NewPost, ThreadKind, ThreadNumber, ThreadOpening};
    let manager = Fixture::new().await;
    let repo = manager.repository("repo").await;
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let selector = SpaceSelector::id(&parent.id);
    let child = (manager.spaces)
        .create_child(
            &*manager,
            &selector,
            "child".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let execution = manager.run_agent(&parent.id).await;
    // The child holds the thread, which its coordinator reads and posts in
    // with `send`, but not from its chat.
    let holder = SpaceSelector::id(&child.id);
    let thread = manager.coordination.open_thread(
        &*manager,
        None,
        ThreadOpening::named("Which database?"),
        Some(ThreadKind::Question),
        Some(&holder),
    );
    let thread = thread.await.unwrap();
    let post = NewPost {
        text: "Which port?".into(),
        ..NewPost::default()
    };
    let number = ThreadNumber::Full(thread.number);
    (manager.coordination)
        .post_to_thread(
            &*manager,
            None,
            number,
            post,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
        .await
        .unwrap();
    let agent = Caller {
        execution: Some(ScopedExecution {
            id: execution,
            kind: shoal_protocol::executions::ExecutionKind::Command,
            agent_execution: None,
        }),
        ..Caller::of_space(parent.id.clone())
    };
    let marked = manager
        .spaces
        .mark_chat_post(&*manager, Some(&agent), &selector, number);
    assert!(marked.await.is_err());
}
