use crate::model::Workspace;
use crate::refusal::Reason;
use crate::scope::Caller;
use crate::stand_in::Fixture;
use shoal_protocol::model::Located;
use shoal_protocol::space_names::SpaceSelector;

/// A registered repository's workspace `name` of each of `names`.
async fn workspaces<const N: usize>(manager: &Fixture, names: [&str; N]) -> [Workspace; N] {
    let repo = manager.repository("repo").await;
    let mut workspaces = Vec::new();
    for name in names {
        let base = Some("HEAD".into());
        let workspace = manager.create_workspace(&repo.id, name.into(), base, None, None);
        workspaces.push(workspace.await.unwrap());
    }
    workspaces.try_into().unwrap()
}

#[tokio::test]
async fn a_space_here_is_located_here_and_one_outside_the_caller_s_scope_is_not() {
    let manager = &Fixture::new().await;
    let [own, other] = workspaces(manager, ["own", "other"]).await;
    let selector = SpaceSelector::id(&other.id);
    let located = manager
        .spaces
        .locate_space(&**manager, None, &selector)
        .await
        .unwrap();
    assert!(matches!(&located, Located::Here(space) if space.id == other.id));
    // A sibling is outside the scope of the other space's process.
    let caller = Caller::of_space(own.id.clone());
    let refused = manager
        .spaces
        .locate_space(&**manager, Some(&caller), &selector)
        .await;
    assert_eq!(Reason::of(&refused.unwrap_err()), Some(Reason::NotFound));
}

#[tokio::test]
async fn a_space_no_machine_has_is_refused_as_not_found() {
    let manager = Fixture::new().await;
    let unknown = SpaceSelector::Number(99.into());
    let refused = manager.spaces.locate_space(&*manager, None, &unknown).await;
    assert_eq!(Reason::of(&refused.unwrap_err()), Some(Reason::NotFound));
}

#[tokio::test]
async fn a_stop_s_selection_takes_the_spaces_below_after_the_one_above_only_when_asked() {
    let manager = &Fixture::new().await;
    // A space beside it stays out.
    let [top, child, _aside] = workspaces(manager, ["top", "child", "aside"]).await;
    let (below, above) = (child.id.clone(), top.id.clone());
    (manager.store)
        .write(move |tx| crate::tree::set_parent(tx, &below, Some(&above)))
        .await
        .unwrap();
    let selected = [SpaceSelector::id(&top.id)];
    let ids = |located: Vec<Located>| -> Vec<_> {
        (located.iter())
            .map(|located| located.id().clone())
            .collect()
    };
    // Named twice, by ID and number, it is taken once.
    let twice = [
        SpaceSelector::id(&top.id),
        SpaceSelector::Number(top.number),
    ];
    let alone = manager
        .spaces
        .select_spaces(&**manager, None, &twice, &[], false)
        .await
        .unwrap();
    assert_eq!(ids(alone), std::slice::from_ref(&top.id));
    let tree = manager
        .spaces
        .select_spaces(&**manager, None, &selected, &[], true)
        .await
        .unwrap();
    assert_eq!(ids(tree), [top.id.clone(), child.id.clone()]);
}

#[tokio::test]
async fn a_chat_in_a_worktree_reaches_neither_its_workspace_nor_itself_on_its_machine() {
    let manager = &Fixture::new().await;
    let [home] = workspaces(manager, ["home"]).await;
    let chat = manager
        .chat_in(&SpaceSelector::id(&home.id), "reader")
        .await
        .unwrap();
    let caller = Caller::of_space(chat.id.clone());
    let reached = manager
        .spaces
        .reached_here(&**manager, Some(&caller))
        .await
        .unwrap();
    let ids: Vec<_> = (reached.iter())
        .map(|located| located.id().clone())
        .collect();
    assert!(
        !ids.contains(&home.id) && !ids.contains(&chat.id),
        "{ids:?}"
    );
}
