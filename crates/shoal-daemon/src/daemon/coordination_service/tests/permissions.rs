//! Effective permissions in a tree of chats and workspaces.
use crate::{
    daemon::{
        manager::Manager,
        test_support::{chat, manager, repository},
    },
    model::Repository,
};

async fn register(temp: &tempfile::TempDir, manager: &Manager, name: &str) -> Repository {
    let checkout = repository(temp.path(), name);
    manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap()
}

#[tokio::test]
async fn another_machine_resolves_a_nested_chat_without_a_repository_as_its_own_does() {
    let (temp, manager) = manager().await;
    let repo = register(&temp, &manager, "repo").await;
    let config = "[modes.orchestrate.permissions]\nuser_holds = 'deny'\n\
                  [modes.chat.permissions]\nuser_holds = 'direct'\nmerge = 'propose'\n";
    (manager.write_local_config(&repo.id, Some(config.into())))
        .await
        .unwrap();
    let master = chat(&manager, "master", "orchestrate", None, None).await;
    let nested = chat(&manager, "nested", "chat", None, Some(&master.id)).await;
    let nested = manager.spaces.space_by_id(&nested.id).await.unwrap();
    let here = manager
        .coordination
        .space_permissions(&*manager, &nested.id, Some(&repo.id));
    let here = here.await.unwrap();
    // What another machine reads of it, resolved in the action's repository.
    let chain = manager
        .coordination
        .chat_modes(&*manager, &nested)
        .await
        .unwrap();
    assert_eq!(chain.len(), 2);
    let elsewhere = manager
        .coordination
        .chained_permissions(&*manager, &chain, &repo.id)
        .await
        .unwrap();
    assert_eq!(elsewhere, here);
}
