use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::test_support::{ProcessGroup, manager, repository},
    removal::{InspectionPolicy, UnsavedWork},
};

#[tokio::test]
async fn a_number_selects_a_workspace_or_a_chat_and_a_command_takes_its_kind() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let worker = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    let process = ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    let chat = manager
        .start_chat(
            repo.id.clone().into(),
            "talk".into(),
            "claude".into(),
            process.id(),
            None,
            false,
        )
        .await
        .unwrap();
    let number = |n: u32| SpaceSelector::Number(n);

    let selected = manager.workspace(&number(worker.number)).await.unwrap();
    assert_eq!(selected.id, worker.id);
    assert_eq!(
        manager.chat(&number(chat.number)).await.unwrap().id,
        chat.id
    );
    assert_eq!(manager.space(&"talk".into()).await.unwrap().id(), chat.id);

    let refused = manager.workspace(&number(chat.number)).await.unwrap_err();
    assert!(refused.to_string().contains("is a chat"), "{refused}");
    assert!(
        refused
            .to_string()
            .contains(&format!("{} (talk)", chat.number))
    );
    assert!(manager.chat(&"worker".into()).await.is_err());
    let missing = manager.space(&number(99)).await.unwrap_err();
    assert!(missing.to_string().contains("no space 99"), "{missing}");
}

#[tokio::test]
async fn a_stored_id_never_selects_a_space_by_name() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    assert!(manager.space(&"worker".into()).await.is_ok());
    assert!(
        manager
            .space(&SpaceSelector::Id("worker".into()))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_removed_workspace_keeps_no_number() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let worker = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    let number = SpaceSelector::Number(worker.number);
    (manager.remove_workspace(&number, UnsavedWork::default(), InspectionPolicy::GitOnly))
        .await
        .unwrap();
    assert!(manager.space(&number).await.is_err());
    assert!(manager.restore_workspace(&number).await.is_err());
    manager.restore_workspace(&"worker".into()).await.unwrap();
}
