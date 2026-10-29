use crate::test_support::{manager, repository};

#[tokio::test]
async fn a_title_is_trimmed_kept_on_the_record_and_cleared() {
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
    assert_eq!(titled.title.as_deref(), Some("Agent tab names"));
    assert_eq!(
        manager.workspace("topic").await.unwrap().title.as_deref(),
        Some("Agent tab names")
    );

    let cleared = manager.set_workspace_title("topic", None).await.unwrap();
    assert_eq!(cleared.title, None);
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
        let error = manager
            .set_workspace_title("topic", Some(title.into()))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("titles must be"), "{error:#}");
    }
    assert_eq!(manager.workspace("topic").await.unwrap().title, None);
}
