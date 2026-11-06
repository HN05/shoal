use crate::daemon::test_support::{manager, repository};

#[tokio::test]
async fn a_title_is_trimmed_kept_on_the_record_and_cleared_after_the_previous_one() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();

    let titled = manager
        .set_workspace_title("topic", Some("  Agent tab names ".into()))
        .await
        .unwrap();
    assert_eq!(titled.workspace.title.as_deref(), Some("Agent tab names"));
    assert_eq!(titled.previous_title, None);
    assert_eq!(
        manager.workspace("topic").await.unwrap().title.as_deref(),
        Some("Agent tab names")
    );

    let cleared = manager.set_workspace_title("topic", None).await.unwrap();
    assert_eq!(cleared.workspace.title, None);
    assert_eq!(cleared.previous_title.as_deref(), Some("Agent tab names"));
}

#[tokio::test]
async fn a_title_that_tabs_cannot_show_is_refused() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();

    for title in ["   ", "two\nlines", &"x".repeat(41)] {
        let titled = manager
            .set_workspace_title("topic", Some(title.into()))
            .await;
        assert!(titled.is_err(), "{title:?}");
    }
    assert_eq!(manager.workspace("topic").await.unwrap().title, None);
}
