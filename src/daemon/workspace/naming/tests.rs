use crate::daemon::test_support::{manager, repository};

#[tokio::test]
async fn a_name_is_cleaned_up_kept_on_the_record_and_returned_with_the_previous_one() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    let selector = crate::client::space_names::SpaceSelector::id(&workspace.id);

    let renamed = manager
        .set_workspace_name(
            &selector,
            &"  Plan:  summary with shoal done ".to_owned().into(),
        )
        .await
        .unwrap();
    assert_eq!(renamed.workspace.name, "Plan: summary with shoal done");
    assert_eq!(renamed.previous_name, "topic");
    // Nothing built from the name moves.
    assert_eq!(renamed.workspace.path, workspace.path);
    assert_eq!(renamed.workspace.branch.as_deref(), Some("topic"));
    assert_eq!(
        manager.workspace(&selector).await.unwrap().name,
        "Plan: summary with shoal done"
    );

    for name in ["   ", "two\nlines", &"x".repeat(41)] {
        let refused = manager
            .set_workspace_name(&selector, &name.to_owned().into())
            .await;
        assert!(refused.is_err(), "{name:?}");
    }
    assert_eq!(
        manager.workspace(&selector).await.unwrap().name,
        "Plan: summary with shoal done"
    );
}
