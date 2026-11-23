//! Taking an action a permission governs: at once, after approval, or not
//! at all.
use crate::{
    config::permissions::PermissionKey,
    daemon::{
        approvals::{Target, Undecided},
        scope::Caller,
        test_support::{chat, manager, repository},
    },
};

fn caller(space: &shoal_ids::space_ids::SpaceId) -> Caller {
    Caller {
        space_id: space.clone(),
        execution: None,
        granted: None,
        read_only: false,
        vouched: None,
    }
}

/// The request `result` waits for or was refused by.
fn undecided(result: anyhow::Result<()>) -> Box<crate::daemon::approvals::ApprovalRequest> {
    let error = result.unwrap_err();
    match error.downcast::<Undecided>() {
        Ok(Undecided(request)) => request,
        Err(error) => panic!("expected an approval request: {error:#}"),
    }
}

#[tokio::test]
async fn releasing_the_user_s_hold_follows_the_user_holds_permission() {
    use crate::model::HoldPlacer;
    use shoal_protocol::space_names::SpaceSelector;

    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = (manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None))
    .await
    .unwrap();
    let master = chat(&manager, "master", "orchestrate", None, None).await;
    let worker = crate::daemon::test_support::workspace_below(
        &manager,
        &repo,
        "worker",
        &master.id,
        Some("worker".into()),
    )
    .await;
    let selector = SpaceSelector::id(&worker.id);
    let hold = || async {
        (manager
            .spaces
            .acquire_hold(&*manager, &selector, "keep".into(), None, HoldPlacer::User))
        .await
        .unwrap();
    };
    hold().await;
    // The worker proposes releasing the user's hold; the master releases it.
    let asking = caller(&worker.id);
    let release =
        manager
            .spaces
            .release_hold_as(&*manager, Some(&asking), &selector, "keep".into());
    let request = undecided(release.await);
    assert_eq!(request.target, Target::Action(PermissionKey::UserHolds));
    (manager.spaces.release_hold_as(
        &*manager,
        Some(&caller(&master.id)),
        &selector,
        "keep".into(),
    ))
    .await
    .unwrap();
    assert!(manager.spaces.holds(&selector).await.unwrap().is_empty());
}

#[tokio::test]
async fn an_approval_to_release_a_hold_covers_only_the_space_it_names() {
    use crate::model::HoldPlacer;
    use shoal_protocol::space_names::SpaceSelector;

    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = (manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None))
    .await
    .unwrap();
    let lead = chat(&manager, "lead", "chat", Some(&repo), None).await;
    let mut held = Vec::new();
    for _ in 0..2 {
        let child =
            crate::daemon::test_support::workspace_below(&manager, &repo, "child", &lead.id, None)
                .await;
        let selector = SpaceSelector::id(&child.id);
        (manager
            .spaces
            .acquire_hold(&*manager, &selector, "keep".into(), None, HoldPlacer::User))
        .await
        .unwrap();
        held.push(selector);
    }
    let leading = caller(&lead.id);
    let release = |index: usize| {
        manager
            .spaces
            .release_hold_as(&*manager, Some(&leading), &held[index], "keep".into())
    };
    let first = undecided(release(0).await);
    (manager.coordination.decide_approval(
        &*manager,
        first.number,
        true,
        None,
        Default::default(),
        None,
    ))
    .await
    .unwrap();
    // The other space of the same name asks for its own approval.
    let second = undecided(release(1).await);
    assert_ne!(second.key, first.key);
    release(0).await.unwrap();
}

#[tokio::test]
async fn administering_shoal_follows_the_admin_permission() {
    use crate::{daemon::scope::Scoped, protocol::RecoveryMethod};

    let (_temp, manager) = manager().await;
    let master = chat(&manager, "master", "orchestrate", None, None).await;
    let worker = chat(&manager, "worker", "chat", None, Some(&master.id)).await;
    let repair = || RecoveryMethod::Reconcile {
        workspace: None,
        options: Default::default(),
    };
    // A master session administers at once; the chat below it asks it.
    (repair().scope(&manager, &caller(&master.id)))
        .await
        .unwrap();
    let mut asking = repair();
    let request = undecided(asking.scope(&manager, &caller(&worker.id)).await.map(drop));
    assert_eq!(request.target, Target::Action(PermissionKey::Admin));
    let routed = manager
        .coordination
        .show_approval(request.number)
        .await
        .unwrap();
    assert_eq!(routed.decider.as_ref(), Some(&master.id));
}

#[tokio::test]
async fn reading_the_user_s_notifications_follows_the_notifications_permission() {
    use crate::{daemon::scope::Scoped, protocol::AlertMethod};

    let (_temp, manager) = manager().await;
    let master = chat(&manager, "master", "orchestrate", None, None).await;
    let below = chat(&manager, "below", "chat", None, Some(&master.id)).await;
    let list = || AlertMethod::ListAlerts {
        to: crate::protocol::AlertsFor::User,
        unread_only: false,
        limit: 10,
    };
    (list().scope(&manager, &caller(&master.id))).await.unwrap();
    // Nothing asks approval for them: a denied space is refused.
    let error = (list().scope(&manager, &caller(&below.id)))
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<Undecided>().is_none(), "{error:#}");
}
