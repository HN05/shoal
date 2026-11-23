//! Taking an action a permission governs: at once, after approval, or not
//! at all.
use std::sync::Arc;

use shoal_daemon_base::permitted_actions::Action;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;

use crate::{
    approvals::{DecisionStatus, Target, Undecided},
    config::permissions::PermissionKey,
    model::Space,
    scope::Caller,
    stand_in::Fixture,
};

/// A registered repository.
struct Registered {
    id: RepositoryId,
}

async fn manager() -> ((), Arc<Fixture>) {
    ((), Arc::new(Fixture::new().await))
}

async fn register(_: &(), manager: &Fixture, name: &str) -> Registered {
    Registered {
        id: manager.repository(name).await,
    }
}

async fn chat(
    manager: &Fixture,
    name: &str,
    mode: &str,
    repository: Option<&Registered>,
    parent: Option<&SpaceId>,
) -> Space {
    let repository = repository.map(|repository| &repository.id);
    let chat = manager.chat(name, mode, repository, parent).await;
    // A chat runs its agent, which decides what is routed to it.
    manager.run_agent(&chat.id);
    chat
}

fn caller(space: &shoal_ids::space_ids::SpaceId) -> Caller {
    Caller {
        space_id: space.clone(),
        execution: None,
        granted: None,
        read_only: false,
        vouched: None,
    }
}

fn restart() -> Action {
    Action::new(PermissionKey::Daemon, "shoal daemon restart")
}

/// The request `result` waits for or was refused by.
fn undecided(result: anyhow::Result<()>) -> Box<crate::approvals::ApprovalRequest> {
    let error = result.unwrap_err();
    match error.downcast::<Undecided>() {
        Ok(Undecided(request)) => request,
        Err(error) => panic!("expected an approval request: {error:#}"),
    }
}

#[tokio::test]
async fn the_permission_takes_refuses_or_proposes_the_action() {
    let (temp, manager) = manager().await;
    let repo = register(&temp, &manager, "repo").await;
    let config = "[modes.plan.permissions]\ndaemon = 'direct'\n\
                  [modes.chat.permissions]\ndaemon = 'propose'\n";
    manager.set_local_config(&repo.id, config);
    let parent = chat(&manager, "parent", "plan", Some(&repo), None).await;
    let child = chat(&manager, "child", "chat", Some(&repo), Some(&parent.id)).await;
    let worker = chat(&manager, "worker", "worker", Some(&repo), None).await;

    // The user and a direct permission act at once; deny refuses.
    manager
        .coordination
        .authorize_action(&**manager, None, restart())
        .await
        .unwrap();
    let direct = caller(&parent.id);
    manager
        .coordination
        .authorize_action(&**manager, Some(&direct), restart())
        .await
        .unwrap();
    let denying = caller(&worker.id);
    let error = (manager
        .coordination
        .authorize_action(&**manager, Some(&denying), restart()))
    .await
    .unwrap_err();
    assert!(error.downcast_ref::<Undecided>().is_none(), "{error:#}");
    assert!(format!("{error:#}").contains("\"deny\""), "{error:#}");

    // Propose asks the parent, which holds the key at direct; asking again
    // finds the same request.
    let proposing = caller(&child.id);
    let request = undecided(
        manager
            .coordination
            .authorize_action(&**manager, Some(&proposing), restart())
            .await,
    );
    assert_eq!(request.target, Target::Action(PermissionKey::Daemon));
    assert_eq!(request.status, DecisionStatus::Pending);
    let again = undecided(
        manager
            .coordination
            .authorize_action(&**manager, Some(&proposing), restart())
            .await,
    );
    assert_eq!(again.key, request.key);
    let routed = manager
        .coordination
        .show_approval(request.number)
        .await
        .unwrap();
    assert_eq!(routed.decider.as_ref(), Some(&parent.id));
    let shown = Some(routed);
    (manager.coordination.decide_routed_approval(
        &**manager,
        &Caller::of_space(parent.id.clone()),
        request.number,
        true,
        shown,
        Default::default(),
    ))
    .await
    .unwrap();

    // The approval lets the same action proceed once.
    (manager
        .coordination
        .authorize_action(&**manager, Some(&proposing), restart()))
    .await
    .unwrap();
    let next = undecided(
        manager
            .coordination
            .authorize_action(&**manager, Some(&proposing), restart())
            .await,
    );
    assert_ne!(next.key, request.key);
    assert_eq!(next.status, DecisionStatus::Pending);
}

#[tokio::test]
async fn a_parent_without_the_key_at_direct_cannot_approve() {
    let (temp, manager) = manager().await;
    let repo = register(&temp, &manager, "repo").await;
    let config = "[permissions]\ndaemon = 'propose'\n";
    manager.set_local_config(&repo.id, config);
    let parent = chat(&manager, "parent", "plan", Some(&repo), None).await;
    let child = chat(&manager, "child", "chat", Some(&repo), Some(&parent.id)).await;
    let request = undecided(
        (manager
            .coordination
            .authorize_action(&**manager, Some(&caller(&child.id)), restart()))
        .await,
    );
    let parent = Caller::of_space(parent.id.clone());
    let refused = manager.coordination.decide_routed_approval(
        &**manager,
        &parent,
        request.number,
        true,
        None,
        Default::default(),
    );
    let error = refused.await.unwrap_err();
    assert!(format!("{error:#}").contains("daemon"), "{error:#}");
    (manager.coordination.decide_routed_approval(
        &**manager,
        &parent,
        request.number,
        false,
        None,
        Default::default(),
    ))
    .await
    .unwrap();
    let denied = undecided(
        manager
            .coordination
            .authorize_action(&**manager, Some(&caller(&child.id)), restart())
            .await,
    );
    assert_eq!(denied.status, DecisionStatus::Denied);
}

#[tokio::test]
async fn an_approved_command_takes_its_permission_s_actions_without_asking_again() {
    let (temp, manager) = manager().await;
    let repo = register(&temp, &manager, "repo").await;
    let config = "[permissions]\ndaemon = 'propose'\n";
    manager.set_local_config(&repo.id, config);
    let chat = chat(&manager, "chat", "chat", Some(&repo), None).await;
    let asking = caller(&chat.id);
    let command = || Action::new(PermissionKey::Daemon, "shoal daemon stop");
    let request = undecided(
        manager
            .coordination
            .grant_action(&**manager, Some(&asking), command())
            .await
            .map(drop),
    );
    (manager.coordination.decide_approval(
        &**manager,
        request.number,
        true,
        None,
        Default::default(),
        None,
    ))
    .await
    .unwrap();
    let token = (manager
        .coordination
        .grant_action(&**manager, Some(&asking), command())
        .await)
        .unwrap()
        .expect("an approval grants a token");

    // The command's own requests take the permission's actions at once,
    // as often as they come, while the space's other processes still ask.
    let granted = manager
        .coordination
        .caller(&**manager, &token)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(granted.space_id, chat.id);
    for _ in 0..2 {
        (manager
            .coordination
            .authorize_action(&**manager, Some(&granted), restart()))
        .await
        .unwrap();
    }
    undecided(
        manager
            .coordination
            .authorize_action(&**manager, Some(&asking), restart())
            .await,
    );
}

#[tokio::test]
async fn a_space_that_decides_as_the_user_decides_what_was_not_routed_to_it() {
    let (_temp, manager) = manager().await;
    let config = "[modes.chat.permissions]\ndaemon = 'propose'\n";
    manager.configure(config);
    let master = chat(&manager, "master", "orchestrate", None, None).await;
    let lead = chat(&manager, "lead", "chat", None, Some(&master.id)).await;
    let worker = chat(&manager, "worker", "chat", None, Some(&lead.id)).await;
    let setup = || Action::new(PermissionKey::Admin, "shoal setup --skip 3");
    let request = undecided(
        manager
            .coordination
            .authorize_action(&**manager, Some(&caller(&worker.id)), setup())
            .await,
    );
    let routed = manager
        .coordination
        .show_approval(request.number)
        .await
        .unwrap();
    assert_eq!(routed.decider.as_ref(), Some(&lead.id));

    // The lead neither decides as the user nor holds admin at direct; the
    // master decides it, though it went to the lead.
    let (leading, mastering) = (caller(&lead.id), caller(&master.id));
    let refused = manager.coordination.decide_approval_as(
        &**manager,
        Some(&leading),
        request.number,
        true,
        None,
        Default::default(),
        None,
    );
    assert!(refused.await.is_err());
    let decided = manager.coordination.decide_approval_as(
        &**manager,
        Some(&mastering),
        request.number,
        true,
        None,
        Default::default(),
        None,
    );
    assert_eq!(decided.await.unwrap().status, DecisionStatus::Approved);
    (manager
        .coordination
        .authorize_action(&**manager, Some(&caller(&worker.id)), setup()))
    .await
    .unwrap();

    // A key it only proposes stays the user's to approve, below it too,
    // though it may deny.
    let restart = || Action::new(PermissionKey::Daemon, "shoal daemon restart");
    let below = undecided(
        manager
            .coordination
            .authorize_action(&**manager, Some(&caller(&worker.id)), restart())
            .await,
    );
    let approving = manager.coordination.decide_approval_as(
        &**manager,
        Some(&mastering),
        below.number,
        true,
        None,
        Default::default(),
        None,
    );
    let refused = format!("{:#}", approving.await.unwrap_err());
    assert!(refused.contains("daemon"), "{refused}");
    let denied = manager.coordination.decide_approval_as(
        &**manager,
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
    let own = undecided(
        manager
            .coordination
            .authorize_action(&**manager, Some(&mastering), own)
            .await,
    );
    assert!(
        !manager
            .coordination
            .decides_as_user(&**manager, &mastering, &own)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn a_chat_without_a_repository_takes_an_action_as_its_repository_allows() {
    let (temp, manager) = manager().await;
    let repo = register(&temp, &manager, "repo").await;
    let config = "[modes.orchestrate.permissions]\nlimits = 'deny'\n";
    manager.set_local_config(&repo.id, config);
    let master = chat(&manager, "master", "orchestrate", None, None).await;
    let start = || "start the agent of worker now".to_owned();
    let mastering = caller(&master.id);
    (manager
        .coordination
        .authorize_beyond_limits(&**manager, &mastering, start(), None))
    .await
    .unwrap();
    let denied = manager.coordination.authorize_beyond_limits(
        &**manager,
        &mastering,
        start(),
        Some(repo.id.clone()),
    );
    let error = denied.await.unwrap_err();
    assert!(error.downcast_ref::<Undecided>().is_none(), "{error:#}");
}

#[tokio::test]
async fn a_decider_without_a_repository_decides_as_the_action_s_repository_allows() {
    let (temp, manager) = manager().await;
    let repo = register(&temp, &manager, "repo").await;
    let config = "[modes.orchestrate.permissions]\nadmin = 'propose'\n";
    manager.set_local_config(&repo.id, config);
    let master = chat(&manager, "master", "orchestrate", None, None).await;
    let below = chat(&manager, "below", "chat", None, Some(&master.id)).await;
    let setup = Action::new(PermissionKey::Admin, "shoal setup --skip 3").in_repository(repo.id);
    let request = undecided(
        manager
            .coordination
            .authorize_action(&**manager, Some(&caller(&below.id)), setup)
            .await,
    );
    // The repository holds the master session's admin to propose, so it
    // may only deny or pass up what its global config would let it approve.
    let mastering = caller(&master.id);
    let approving = manager.coordination.decide_approval_as(
        &**manager,
        Some(&mastering),
        request.number,
        true,
        None,
        Default::default(),
        None,
    );
    let refused = format!("{:#}", approving.await.unwrap_err());
    assert!(refused.contains("admin"), "{refused}");
}
