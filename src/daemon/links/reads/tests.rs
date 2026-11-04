use crate::{
    protocol::ItemContext,
    test_support::{ProcessGroup, git, manager, repository},
};

#[tokio::test]
async fn a_stopped_chat_reads_in_its_repositorys_checkout() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let origin = "https://github.com/team/repo";
    git(&checkout, &["remote", "add", "origin", origin]);
    let process = ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    let chat = manager
        .start_chat(
            repo.id.into(),
            "talk".into(),
            "claude".into(),
            process.id(),
            None,
            false,
        )
        .await
        .unwrap();
    process.kill();
    manager.stop_finished_chats().await.unwrap();
    assert!(manager.running_chat(&chat.id).await.unwrap().is_none());

    let forge = manager
        .context_forge(&ItemContext::Chat(chat.id))
        .await
        .unwrap();
    assert!(forge == manager.forge(origin).await.unwrap());
}
