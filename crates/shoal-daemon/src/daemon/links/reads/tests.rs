use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::test_support::{ProcessGroup, git, manager, repository},
    forge::items::link::{LinkKind, Selection},
    protocol::ItemContext,
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
    assert!(
        manager
            .running_chat(&SpaceSelector::id(&chat.id))
            .await
            .is_err()
    );

    let forge = manager
        .context_forge(&ItemContext::Chat(SpaceSelector::id(&chat.id)))
        .await
        .unwrap();
    assert!(forge == manager.forge(origin).await.unwrap());
}

#[tokio::test]
async fn a_checkout_without_an_origin_selects_an_item_by_its_url() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let url = "https://github.com/team/other/issues/7";
    let selection = Selection {
        kind: Some(LinkKind::Issue),
        input: Some(url.into()),
    };

    let selected = manager
        .select_context_items(&ItemContext::Checkout(repo.id), &selection, true, false)
        .await
        .unwrap();
    assert_eq!(selected.items.len(), 1);
    assert_eq!(selected.items[0].url, url);
}
