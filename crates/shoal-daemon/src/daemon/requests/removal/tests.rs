//! Removal requests from a scoped caller.
use super::*;
use crate::{
    daemon::test_support::{chat, manager, repository, workspace_below},
    model::HoldPlacer,
    removal::UnsavedWork,
};
use shoal_ids::space_ids::SpaceId;

fn caller(space: &SpaceId) -> Caller {
    Caller {
        space_id: space.clone(),
        execution: None,
        granted: None,
        read_only: false,
        vouched: None,
    }
}

/// A master session, whose `user_holds` permission is `direct`, removes the
/// chat and the workspace below it that the user holds, releasing the holds.
#[tokio::test]
async fn a_scoped_removal_takes_the_user_s_holds_as_user_holds_allows() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = (manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None))
    .await
    .unwrap();
    let master = chat(&manager, "master", "orchestrate", None, None).await;
    let notes = chat(&manager, "notes", "chat", Some(&repo), Some(&master.id)).await;
    let worker = workspace_below(&manager, &repo, "worker", &master.id, None).await;
    let scoped = caller(&master.id);
    for id in [&notes.id, &worker.id] {
        let selector = SpaceSelector::id(id);
        (manager.acquire_hold(&selector, "keep".into(), None, HoldPlacer::User))
            .await
            .unwrap();
        let mut method = RemovalMethod::RemoveSpace {
            space: selector,
            unsaved: UnsavedWork::default(),
            keep_user_holds: false,
            above: None,
        };
        method.scope(&manager, &scoped).await.unwrap();
        handle(&manager, method, Some(&scoped)).await.unwrap();
        assert!(manager.find_space(id).await.unwrap().is_none());
    }
}
