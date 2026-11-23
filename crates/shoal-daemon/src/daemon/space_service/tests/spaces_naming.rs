use crate::daemon::test_support::{manager, repository};

#[tokio::test]
async fn a_request_cannot_create_a_space_under_a_name_no_one_could_set() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    // Requests read names verbatim, as stored records do.
    for name in ["   ", "two\nlines", "a\u{202e}b", &"x".repeat(41)] {
        let options = crate::protocol::CreationOptions {
            name: Some(name.to_owned().into()),
            ..Default::default()
        };
        let new = crate::daemon::workspace::NewWorkspace {
            options,
            ..Default::default()
        };
        let refused = manager
            .spaces
            .create_workspace_with(&*manager, &repo.id, "topic".into(), None, new)
            .await;
        assert!(refused.is_err(), "{name:?}");
        let chat = manager
            .start_chat(
                repo.id.clone().into(),
                name.to_owned().into(),
                "claude".into(),
                None,
            )
            .await;
        assert!(chat.is_err(), "{name:?}");
    }
    assert!(manager.spaces.list_workspaces().await.unwrap().is_empty());
}
