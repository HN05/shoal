use super::*;
use crate::daemon::test_support::{chat, manager, repository, workspace_below};

#[tokio::test]
async fn a_scoped_caller_gives_at_most_its_own_spaces_priority() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (top, _process) = chat(&manager, "top", "chat", Some(&repo), None).await;
    let child = workspace_below(&manager, &repo, "child", &top.id, None).await;
    let (master, _master) = chat(&manager, "master", "orchestrate", None, None).await;
    let caller = |space_id: &SpaceId| Caller {
        space_id: space_id.clone(),
        execution: None,
    };
    let selector = SpaceSelector::id(&child.id);
    let urgent = |caller: Option<Caller>| {
        let (manager, selector) = (manager.clone(), selector.clone());
        async move { (manager.set_priority(caller.as_ref(), &selector, Priority::URGENT)).await }
    };

    // The parent's own priority is the default, below urgent.
    assert!(urgent(Some(caller(&top.id))).await.is_err());
    (manager.set_priority(Some(&caller(&top.id)), &selector, Priority::LOW))
        .await
        .unwrap();
    // The user and a master session give any value.
    assert_eq!(urgent(None).await.unwrap().priority, Priority::URGENT);
    (manager.set_priority(None, &SpaceSelector::id(&top.id), Priority::LOW))
        .await
        .unwrap();
    let highest = Priority::try_from(100).unwrap();
    (manager.set_priority(Some(&caller(&master.id)), &selector, highest))
        .await
        .unwrap();
    let id = child.id.clone();
    let stored = manager.store.read(move |tx| current(tx, &id)).await;
    assert_eq!(stored.unwrap(), highest);
}
