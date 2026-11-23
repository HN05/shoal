use crate::daemon::test_support::{manager, repository};
use shoal_refs::BranchName;

#[tokio::test]
async fn a_name_is_cleaned_up_kept_on_the_record_and_returned_with_the_previous_one() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    let selector = shoal_protocol::space_names::SpaceSelector::id(&workspace.id);

    let renamed = manager
        .spaces
        .rename_space(
            &selector,
            &"  Plan:  summary with shoal done ".to_owned().into(),
        )
        .await
        .unwrap();
    let Some(renamed_workspace) = renamed.space.to_workspace() else {
        panic!("renamed a chat");
    };
    assert_eq!(renamed_workspace.name, "Plan: summary with shoal done");
    assert_eq!(renamed.previous_name, "topic");
    // Nothing built from the name moves.
    assert_eq!(renamed_workspace.path, workspace.path);
    assert_eq!(
        renamed_workspace.branch().map(BranchName::as_str),
        Some("topic")
    );
    assert_eq!(
        manager.spaces.workspace(&selector).await.unwrap().name,
        "Plan: summary with shoal done"
    );

    for name in ["   ", "two\nlines", &"x".repeat(41)] {
        let refused = manager
            .spaces
            .rename_space(&selector, &name.to_owned().into())
            .await;
        assert!(refused.is_err(), "{name:?}");
    }
    assert_eq!(
        manager.spaces.workspace(&selector).await.unwrap().name,
        "Plan: summary with shoal done"
    );
}

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
