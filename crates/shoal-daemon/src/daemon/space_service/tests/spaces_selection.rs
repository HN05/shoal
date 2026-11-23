use crate::{
    daemon::test_support::{manager, repository},
    removal::{InspectionPolicy, UnsavedWork},
};
use shoal_protocol::space_names::SpaceSelector;

#[tokio::test]
async fn a_number_selects_a_workspace_or_a_chat_and_a_command_takes_its_kind() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let worker = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(repo.id.clone().into(), "talk".into(), "claude".into(), None)
        .await
        .unwrap();
    let number = SpaceSelector::Number;

    let selected = manager
        .spaces
        .workspace(&number(worker.number))
        .await
        .unwrap();
    assert_eq!(selected.id, worker.id);
    assert_eq!(
        manager.spaces.chat(&number(chat.number)).await.unwrap().id,
        chat.id
    );
    assert_eq!(
        manager
            .spaces
            .space(&manager.named("talk").await)
            .await
            .unwrap()
            .id,
        chat.id
    );

    let refused = manager
        .spaces
        .workspace(&number(chat.number))
        .await
        .unwrap_err();
    assert!(refused.to_string().contains("is a chat"), "{refused}");
    assert!(
        refused
            .to_string()
            .contains(&format!("{} (talk)", chat.number))
    );
    assert!(
        manager
            .spaces
            .chat(&manager.named("worker").await)
            .await
            .is_err()
    );
    let missing = manager.spaces.space(&number(99.into())).await.unwrap_err();
    assert!(missing.to_string().contains("no space 99"), "{missing}");
}

#[tokio::test]
async fn a_removed_space_s_id_names_the_repository_removed_with_it() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let removed = manager
        .create_workspace(&repo.id, "removed".into(), None, None, None)
        .await
        .unwrap();
    let taken = manager
        .create_workspace(&repo.id, "taken".into(), None, None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat_in_workspace(
            &SpaceSelector::id(&taken.id),
            "below".into(),
            "claude".into(),
            None,
        )
        .await
        .unwrap();
    let removed = SpaceSelector::id(&removed.id);
    (manager.spaces.remove_selected(
        &*manager,
        &removed,
        UnsavedWork::default(),
        InspectionPolicy::GitOnly,
    ))
    .await
    .unwrap();
    manager
        .repositories
        .remove(&*manager, &repo.id)
        .await
        .unwrap();

    let refused = manager
        .spaces
        .space(&removed)
        .await
        .unwrap_err()
        .to_string();
    assert!(refused.contains("was removed"), "{refused}");
    assert!(!refused.contains("repository"), "{refused}");
    let refused = manager
        .spaces
        .space_by_id(&taken.id)
        .await
        .unwrap_err()
        .to_string();
    let repository = format!("removed with its repository {}", repo.id);
    assert!(refused.contains(&repository), "{refused}");
    let refused = manager.spaces.workspace_by_id(&taken.id).await.unwrap_err();
    assert!(refused.to_string().contains(&repository), "{refused}");
    // A chat below goes with the repository too.
    let refused = manager
        .spaces
        .space_by_id(&chat.id)
        .await
        .unwrap_err()
        .to_string();
    assert!(refused.contains(&repository), "{refused}");
}
