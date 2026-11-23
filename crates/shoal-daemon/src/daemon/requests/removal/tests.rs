//! Removal requests from a scoped caller.
use super::*;
use crate::daemon::scope::Access;
use crate::{
    daemon::test_support::{chat, manager, repository, vouched_admin, workspace_below},
    model::HoldPlacer,
    removal::UnsavedWork,
};
use shoal_ids::space_ids::SpaceId;

fn caller(space: &SpaceId) -> Caller {
    Caller {
        space_id: space.clone(),
        execution: None,
        granted: None,
        access: Access::Full,
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
        (manager
            .spaces
            .acquire_hold(&*manager, &selector, "keep".into(), None, HoldPlacer::User))
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
        assert!(manager.spaces.find_space(id).await.unwrap().is_none());
    }
}

/// A caller on another machine reads the package of a removed space's
/// record kept here where it may restore it, as its `admin` permission
/// allows; one whose scope misses the record's parent is refused.
#[tokio::test]
async fn a_removed_record_s_package_goes_to_a_caller_that_may_restore_it() {
    use crate::removal::InspectionPolicy;
    use shoal_protocol::config::permissions::Acting;

    let (_root, manager) = manager().await;
    let planner = chat(&manager, "planner", "chat", None, None).await;
    let notes = chat(&manager, "notes", "chat", None, Some(&planner.id)).await;
    let other = chat(&manager, "other", "chat", None, None).await;
    let selector = SpaceSelector::id(&notes.id);
    (manager.spaces.remove_selected(
        &*manager,
        &selector,
        UnsavedWork::default(),
        InspectionPolicy::GitOnly,
    ))
    .await
    .unwrap();
    let reading = || RemovalMethod::ReadHeldPackage {
        space: selector.clone(),
        copy: true,
    };
    let direct = vouched_admin(&manager, &planner.id, Acting::Direct).await;
    let denied = vouched_admin(&manager, &planner.id, Acting::Deny).await;

    let refused = async |caller: &Caller| reading().scope(&manager, caller).await.is_err();
    assert!(reading().scope(&manager, &direct).await.unwrap().is_none());
    assert!(refused(&denied).await);
    assert!(refused(&caller(&other.id)).await);
}
