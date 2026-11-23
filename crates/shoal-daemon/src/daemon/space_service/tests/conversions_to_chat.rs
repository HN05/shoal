use crate::{
    daemon::{
        manager::Manager,
        test_support::{manager, repository},
    },
    model::Repository,
};
use shoal_protocol::space_names::SpaceSelector;

async fn registered(root: &std::path::Path, manager: &Manager) -> Repository {
    let path = repository(root, "repo");
    manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap()
}

#[tokio::test]
async fn converting_a_chat_refuses_it_as_no_workspace() {
    let (root, manager) = manager().await;
    let repo = registered(root.path(), &manager).await;
    let talk = crate::daemon::test_support::chat(&manager, "talk", "chat", Some(&repo), None).await;

    let refused = manager
        .spaces
        .convert_to_chat(
            &*manager,
            &SpaceSelector::id(&talk.id),
            crate::model::chat_mode(),
        )
        .await
        .unwrap_err();

    let reason = crate::refusal::Reason::of(&refused);
    assert_eq!(
        reason,
        Some(crate::refusal::Reason::NotFound),
        "{refused:#}"
    );
}
