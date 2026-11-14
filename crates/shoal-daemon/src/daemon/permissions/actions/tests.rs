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
        granted: None,
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

#[tokio::test]
async fn an_approved_command_takes_its_permission_s_actions_without_asking_again() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = (manager.register_repository(checkout.to_str().unwrap().into(), None, None))
        .await
        .unwrap();
    let config = "[permissions]\ndaemon = 'propose'\n";
    (manager.write_local_config(&repo.id, Some(config.into())))
        .await
        .unwrap();
    let chat = chat(&manager, "chat", "chat", Some(&repo), None).await;
    let asking = caller(&chat.id);
    let command = || Action::new(PermissionKey::Daemon, "shoal daemon stop");
    let request = undecided(
        manager
            .grant_action(Some(&asking), command())
            .await
            .map(drop),
    );
    (manager.decide_approval(request.number, true, None, Default::default(), None))
        .await
        .unwrap();
    let token = (manager.grant_action(Some(&asking), command()).await)
        .unwrap()
        .expect("an approval grants a token");

    // The command's own requests take the permission's actions at once,
    // as often as they come, while the space's other processes still ask.
    let granted = manager.caller(&token).await.unwrap().unwrap();
    assert_eq!(granted.space_id, chat.id);
    for _ in 0..2 {
        (manager.authorize_action(Some(&granted), restart()))
            .await
            .unwrap();
    }
    undecided(manager.authorize_action(Some(&asking), restart()).await);
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
    let routed = manager.show_approval(request.number).await.unwrap();
    assert_eq!(routed.decider.as_ref(), Some(&master.id));
}

#[tokio::test]
async fn releasing_the_user_s_hold_follows_the_user_holds_permission() {
    use crate::{client::space_names::SpaceSelector, model::HoldPlacer};

    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = (manager.register_repository(checkout.to_str().unwrap().into(), None, None))
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
        (manager.acquire_hold(&selector, "keep".into(), None, HoldPlacer::User))
            .await
            .unwrap();
    };
    hold().await;
    // The worker proposes releasing the user's hold; the master releases it.
    let asking = caller(&worker.id);
    let release = manager.release_hold_as(Some(&asking), &selector, "keep".into());
    let request = undecided(release.await);
    assert_eq!(request.target, Target::Action(PermissionKey::UserHolds));
    (manager.release_hold_as(Some(&caller(&master.id)), &selector, "keep".into()))
        .await
        .unwrap();
    assert!(manager.holds(&selector).await.unwrap().is_empty());
}

#[tokio::test]
async fn reading_the_user_s_notifications_follows_the_notifications_permission() {
    use crate::{daemon::scope::Scoped, protocol::NotificationMethod};

    let (_temp, manager) = manager().await;
    let master = chat(&manager, "master", "orchestrate", None, None).await;
    let below = chat(&manager, "below", "chat", None, Some(&master.id)).await;
    let list = || NotificationMethod::ListNotifications {
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

#[tokio::test]
async fn a_space_that_decides_as_the_user_decides_what_was_not_routed_to_it() {
    let (_temp, manager) = manager().await;
    let config = "[modes.chat.permissions]\ndaemon = 'propose'\n";
    crate::daemon::test_support::configure(&manager, config).await;
    let master = chat(&manager, "master", "orchestrate", None, None).await;
    let lead = chat(&manager, "lead", "chat", None, Some(&master.id)).await;
    let worker = chat(&manager, "worker", "chat", None, Some(&lead.id)).await;
    let setup = || Action::new(PermissionKey::Admin, "shoal setup --skip 3");
    let request = undecided(
        manager
            .authorize_action(Some(&caller(&worker.id)), setup())
            .await,
    );
    let routed = manager.show_approval(request.number).await.unwrap();
    assert_eq!(routed.decider.as_ref(), Some(&lead.id));

    // The lead neither decides as the user nor holds admin at direct; the
    // master decides it, though it went to the lead.
    let (leading, mastering) = (caller(&lead.id), caller(&master.id));
    let refused = manager.decide_approval_as(
        Some(&leading),
        request.number,
        true,
        None,
        Default::default(),
        None,
    );
    assert!(refused.await.is_err());
    let decided = manager.decide_approval_as(
        Some(&mastering),
        request.number,
        true,
        None,
        Default::default(),
        None,
    );
    assert_eq!(decided.await.unwrap().status, DecisionStatus::Approved);
    (manager.authorize_action(Some(&caller(&worker.id)), setup()))
        .await
        .unwrap();

    // A key it only proposes stays the user's to approve, below it too,
    // though it may deny.
    let restart = || Action::new(PermissionKey::Daemon, "shoal daemon restart");
    let below = undecided(
        manager
            .authorize_action(Some(&caller(&worker.id)), restart())
            .await,
    );
    let approving = manager.decide_approval_as(
        Some(&mastering),
        below.number,
        true,
        None,
        Default::default(),
        None,
    );
    let refused = format!("{:#}", approving.await.unwrap_err());
    assert!(refused.contains("daemon"), "{refused}");
    let denied = manager.decide_approval_as(
        Some(&mastering),
        below.number,
        false,
        None,
        Default::default(),
        None,
    );
    assert_eq!(denied.await.unwrap().status, DecisionStatus::Denied);

    // Its own requests stay another's to decide.
    let own = Action::new(PermissionKey::Daemon, "shoal daemon restart");
    let own = undecided(manager.authorize_action(Some(&mastering), own).await);
    assert!(!manager.decides_as_user(&mastering, &own).await.unwrap());
}

#[tokio::test]
async fn approving_ci_runs_follows_the_ci_approve_permission() {
    use crate::{
        client::space_names::SpaceSelector,
        daemon::scope::Scoped,
        protocol::{CiMethod, ItemContext},
    };

    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    crate::daemon::test_support::git(
        &checkout,
        &["remote", "add", "origin", "https://github.com/team/repo"],
    );
    let repo = (manager.register_repository(checkout.to_str().unwrap().into(), None, None))
        .await
        .unwrap();
    let master = chat(&manager, "master", "orchestrate", None, None).await;
    let worker = chat(&manager, "worker", "chat", Some(&repo), Some(&master.id)).await;
    let mut approve = CiMethod::ApproveCiRuns {
        context: ItemContext::Chat(SpaceSelector::id(&worker.id)),
        run: Some("7".parse().unwrap()),
        pr: None,
    };
    let request = undecided(approve.scope(&manager, &caller(&worker.id)).await.map(drop));
    assert_eq!(request.target, Target::Action(PermissionKey::CiApprove));
}

#[tokio::test]
async fn deleting_a_pr_s_branch_follows_the_merge_permission() {
    use crate::{
        client::space_names::SpaceSelector,
        daemon::scope::Scoped,
        forge::items::{action::Action as ItemAction, link::ItemKind},
        protocol::{ItemContext, ItemMethod},
    };

    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    crate::daemon::test_support::git(
        &checkout,
        &["remote", "add", "origin", "https://github.com/team/repo"],
    );
    let repo = (manager.register_repository(checkout.to_str().unwrap().into(), None, None))
        .await
        .unwrap();
    let chat = chat(&manager, "chat", "chat", Some(&repo), None).await;
    let mut delete = ItemMethod::ItemAction {
        context: ItemContext::Chat(SpaceSelector::id(&chat.id)),
        kind: ItemKind::Pr,
        item: Some("12".parse().unwrap()),
        action: ItemAction::DeleteBranch,
        confirmed: None,
    };
    let request = undecided(delete.scope(&manager, &caller(&chat.id)).await.map(drop));
    assert_eq!(request.target, Target::Action(PermissionKey::Merge));
}

#[test]
fn a_long_request_names_its_start_and_a_digest_of_all_of_it() {
    use crate::forge::raw::{HttpMethod, RawRequest};

    let write = |body: &str| RawRequest {
        method: HttpMethod::Patch,
        path: "repos/{owner}/{repo}/issues/3".into(),
        body: Some(serde_json::json!({ "body": body.repeat(100) })),
    };
    let named = |body| Action::request(PermissionKey::ApiWrites, &write(body), None).action;
    let (first, other) = (named("a"), named("b"));
    assert!(
        first.starts_with(r#"{"method":"PATCH","path":"repos/{owner}/{repo}/issues/3""#),
        "{first}"
    );
    assert_ne!(first, other);
}
