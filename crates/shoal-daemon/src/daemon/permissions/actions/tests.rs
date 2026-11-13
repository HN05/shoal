//! Taking an action a permission governs: at once, after approval, or not
//! at all.
use crate::{
    config::permissions::PermissionKey,
    daemon::{
        approvals::{DecisionStatus, Target, Undecided},
        permissions::Action,
        scope::Caller,
        test_support::{chat, manager, repository},
    },
};

fn caller(space: &crate::client::space_ids::SpaceId) -> Caller {
    Caller {
        space_id: space.clone(),
        execution: None,
    }
}

fn restart() -> Action {
    Action::new(PermissionKey::Daemon, "shoal daemon restart")
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
async fn the_permission_takes_refuses_or_proposes_the_action() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = (manager.register_repository(checkout.to_str().unwrap().into(), None, None))
        .await
        .unwrap();
    let config = "[modes.plan.permissions]\ndaemon = 'direct'\n\
                  [modes.chat.permissions]\ndaemon = 'propose'\n";
    (manager.write_local_config(&repo.id, Some(config.into())))
        .await
        .unwrap();
    let parent = chat(&manager, "parent", "plan", Some(&repo), None).await;
    let child = chat(&manager, "child", "chat", Some(&repo), Some(&parent.id)).await;
    let worker = chat(&manager, "worker", "worker", Some(&repo), None).await;

    // The user and a direct permission act at once; deny refuses.
    manager.authorize_action(None, restart()).await.unwrap();
    let direct = caller(&parent.id);
    manager
        .authorize_action(Some(&direct), restart())
        .await
        .unwrap();
    let denying = caller(&worker.id);
    let error = (manager.authorize_action(Some(&denying), restart()))
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<Undecided>().is_none(), "{error:#}");
    assert!(format!("{error:#}").contains("\"deny\""), "{error:#}");

    // Propose asks the parent, which holds the key at direct; asking again
    // finds the same request.
    let proposing = caller(&child.id);
    let request = undecided(manager.authorize_action(Some(&proposing), restart()).await);
    assert_eq!(request.target, Target::Action(PermissionKey::Daemon));
    assert_eq!(request.status, DecisionStatus::Pending);
    let again = undecided(manager.authorize_action(Some(&proposing), restart()).await);
    assert_eq!(again.key, request.key);
    let routed = manager.show_approval(request.number).await.unwrap();
    assert_eq!(routed.decider.as_ref(), Some(&parent.id));
    let shown = Some(routed);
    (manager.decide_routed_approval(&parent.id, request.number, true, shown, Default::default()))
        .await
        .unwrap();

    // The approval lets the same action proceed once.
    (manager.authorize_action(Some(&proposing), restart()))
        .await
        .unwrap();
    let next = undecided(manager.authorize_action(Some(&proposing), restart()).await);
    assert_ne!(next.key, request.key);
    assert_eq!(next.status, DecisionStatus::Pending);
}

#[tokio::test]
async fn a_parent_without_the_key_at_direct_cannot_approve() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = (manager.register_repository(checkout.to_str().unwrap().into(), None, None))
        .await
        .unwrap();
    let config = "[permissions]\ndaemon = 'propose'\n";
    (manager.write_local_config(&repo.id, Some(config.into())))
        .await
        .unwrap();
    let parent = chat(&manager, "parent", "plan", Some(&repo), None).await;
    let child = chat(&manager, "child", "chat", Some(&repo), Some(&parent.id)).await;
    let request = undecided((manager.authorize_action(Some(&caller(&child.id)), restart())).await);
    let refused =
        manager.decide_routed_approval(&parent.id, request.number, true, None, Default::default());
    let error = refused.await.unwrap_err();
    assert!(format!("{error:#}").contains("daemon"), "{error:#}");
    (manager.decide_routed_approval(&parent.id, request.number, false, None, Default::default()))
        .await
        .unwrap();
    let denied = undecided(
        manager
            .authorize_action(Some(&caller(&child.id)), restart())
            .await,
    );
    assert_eq!(denied.status, DecisionStatus::Denied);
}
