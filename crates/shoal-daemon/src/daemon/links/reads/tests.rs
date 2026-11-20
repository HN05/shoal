use crate::{
    daemon::test_support::{git, manager, repository},
    forge::items::link::{LinkKind, Selection},
    protocol::ItemContext,
};
use shoal_protocol::space_names::SpaceSelector;

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
    let chat = manager
        .start_chat(repo.id.into(), "talk".into(), "claude".into(), None)
        .await
        .unwrap();
    manager.end_executions(&chat.id).await;

    let forge = manager
        .links
        .context_forge(&*manager, &ItemContext::space(SpaceSelector::id(&chat.id)))
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
        input: Some(url.parse().unwrap()),
    };

    let selected = manager
        .links
        .select_context_items(
            &*manager,
            &ItemContext::Checkout(repo.id).into(),
            &selection,
            true,
            None,
        )
        .await
        .unwrap();
    assert_eq!(selected.items.len(), 1);
    assert_eq!(selected.items[0].url, url.parse().unwrap());
}

#[tokio::test]
async fn a_view_of_nothing_needs_no_forge_behind_a_local_origin() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let bare = root.path().join("origin.git");
    git(root.path(), &["init", "--bare", bare.to_str().unwrap()]);
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "local".into(), None, None, None)
        .await
        .unwrap();
    git(
        &checkout,
        &["remote", "add", "origin", bare.to_str().unwrap()],
    );
    let target = ItemContext::space(SpaceSelector::id(&workspace.id)).into();

    let viewed = manager
        .view_items(&target, &Selection::default(), false, None)
        .await
        .unwrap();
    assert!(viewed.items.is_empty());
}
