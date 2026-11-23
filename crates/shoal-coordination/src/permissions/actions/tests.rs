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

async fn chat(
    fixture: &Fixture,
    name: &str,
    mode: &str,
    repository: Option<&RepositoryId>,
    parent: Option<&SpaceId>,
) -> Space {
    let chat = fixture.chat(name, mode, repository, parent).await;
    // A chat runs its agent, which decides what is routed to it.
    fixture.run_agent(&chat.id);
    chat
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
    let fixture = Arc::new(Fixture::new().await);
    let repo = fixture.repository("repo").await;
    let config = "[modes.plan.permissions]\ndaemon = 'direct'\n\
                  [modes.chat.permissions]\ndaemon = 'propose'\n";
    fixture.set_local_config(&repo, config);
    let parent = chat(&fixture, "parent", "plan", Some(&repo), None).await;
    let child = chat(&fixture, "child", "chat", Some(&repo), Some(&parent.id)).await;
    let worker = chat(&fixture, "worker", "worker", Some(&repo), None).await;

    // The user and a direct permission act at once; deny refuses.
    fixture
        .coordination
        .authorize_action(&**fixture, None, restart())
        .await
        .unwrap();
    let direct = Fixture::caller(&parent.id);
    fixture
        .coordination
        .authorize_action(&**fixture, Some(&direct), restart())
        .await
        .unwrap();
    let denying = Fixture::caller(&worker.id);
    let error = (fixture
        .coordination
        .authorize_action(&**fixture, Some(&denying), restart()))
    .await
    .unwrap_err();
    assert!(error.downcast_ref::<Undecided>().is_none(), "{error:#}");
    assert!(format!("{error:#}").contains("\"deny\""), "{error:#}");

    // Propose asks the parent, which holds the key at direct; asking again
    // finds the same request.
    let proposing = Fixture::caller(&child.id);
    let request = undecided(
        fixture
            .coordination
            .authorize_action(&**fixture, Some(&proposing), restart())
            .await,
    );
    assert_eq!(request.target, Target::Action(PermissionKey::Daemon));
    assert_eq!(request.status, DecisionStatus::Pending);
    let again = undecided(
        fixture
            .coordination
            .authorize_action(&**fixture, Some(&proposing), restart())
            .await,
    );
    assert_eq!(again.key, request.key);
    let routed = fixture
        .coordination
        .show_approval(request.number)
        .await
        .unwrap();
    assert_eq!(routed.decider.as_ref(), Some(&parent.id));
    let shown = Some(routed);
    (fixture.coordination.decide_routed_approval(
        &**fixture,
        &Caller::of_space(parent.id.clone()),
        request.number,
        true,
        shown,
        Default::default(),
    ))
    .await
    .unwrap();

    // The approval lets the same action proceed once.
    (fixture
        .coordination
        .authorize_action(&**fixture, Some(&proposing), restart()))
    .await
    .unwrap();
    let next = undecided(
        fixture
            .coordination
            .authorize_action(&**fixture, Some(&proposing), restart())
            .await,
    );
    assert_ne!(next.key, request.key);
    assert_eq!(next.status, DecisionStatus::Pending);
}

#[tokio::test]
async fn a_parent_without_the_key_at_direct_cannot_approve() {
    let fixture = Arc::new(Fixture::new().await);
    let repo = fixture.repository("repo").await;
    let config = "[permissions]\ndaemon = 'propose'\n";
    fixture.set_local_config(&repo, config);
    let parent = chat(&fixture, "parent", "plan", Some(&repo), None).await;
    let child = chat(&fixture, "child", "chat", Some(&repo), Some(&parent.id)).await;
    let request = undecided(
        (fixture.coordination.authorize_action(
            &**fixture,
            Some(&Fixture::caller(&child.id)),
            restart(),
        ))
        .await,
    );
    let parent = Caller::of_space(parent.id.clone());
    let refused = fixture.coordination.decide_routed_approval(
        &**fixture,
        &parent,
        request.number,
        true,
        None,
        Default::default(),
    );
    let error = refused.await.unwrap_err();
    assert!(format!("{error:#}").contains("daemon"), "{error:#}");
    (fixture.coordination.decide_routed_approval(
        &**fixture,
        &parent,
        request.number,
        false,
        None,
        Default::default(),
    ))
    .await
    .unwrap();
    let denied = undecided(
        fixture
            .coordination
            .authorize_action(&**fixture, Some(&Fixture::caller(&child.id)), restart())
            .await,
    );
    assert_eq!(denied.status, DecisionStatus::Denied);
}

#[tokio::test]
async fn an_approved_command_takes_its_permission_s_actions_without_asking_again() {
    let fixture = Arc::new(Fixture::new().await);
    let repo = fixture.repository("repo").await;
    let config = "[permissions]\ndaemon = 'propose'\n";
    fixture.set_local_config(&repo, config);
    let chat = chat(&fixture, "chat", "chat", Some(&repo), None).await;
    let asking = Fixture::caller(&chat.id);
    let command = || Action::new(PermissionKey::Daemon, "shoal daemon stop");
    let request = undecided(
        fixture
            .coordination
            .grant_action(&**fixture, Some(&asking), command())
            .await
            .map(drop),
    );
    (fixture.coordination.decide_approval(
        &**fixture,
        request.number,
        true,
        None,
        Default::default(),
        None,
    ))
    .await
    .unwrap();
    let token = (fixture
        .coordination
        .grant_action(&**fixture, Some(&asking), command())
        .await)
        .unwrap()
        .expect("an approval grants a token");

    // The command's own requests take the permission's actions at once,
    // as often as they come, while the space's other processes still ask.
    let granted = fixture
        .coordination
        .caller(&**fixture, &token)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(granted.space_id, chat.id);
    for _ in 0..2 {
        (fixture
            .coordination
            .authorize_action(&**fixture, Some(&granted), restart()))
        .await
        .unwrap();
    }
    undecided(
        fixture
            .coordination
            .authorize_action(&**fixture, Some(&asking), restart())
            .await,
    );
}

#[tokio::test]
async fn a_space_that_decides_as_the_user_decides_what_was_not_routed_to_it() {
    let fixture = Arc::new(Fixture::new().await);
    let config = "[modes.chat.permissions]\ndaemon = 'propose'\n";
    fixture.configure(config);
    let master = chat(&fixture, "master", "orchestrate", None, None).await;
    let lead = chat(&fixture, "lead", "chat", None, Some(&master.id)).await;
    let worker = chat(&fixture, "worker", "chat", None, Some(&lead.id)).await;
    let setup = || Action::new(PermissionKey::Admin, "shoal setup --skip 3");
    let request = undecided(
        fixture
            .coordination
            .authorize_action(&**fixture, Some(&Fixture::caller(&worker.id)), setup())
            .await,
    );
    let routed = fixture
        .coordination
        .show_approval(request.number)
        .await
        .unwrap();
    assert_eq!(routed.decider.as_ref(), Some(&lead.id));

    // The lead neither decides as the user nor holds admin at direct; the
    // master decides it, though it went to the lead.
    let (leading, mastering) = (Fixture::caller(&lead.id), Fixture::caller(&master.id));
    let refused = fixture.coordination.decide_approval_as(
        &**fixture,
        Some(&leading),
        request.number,
        true,
        None,
        Default::default(),
        None,
    );
    assert!(refused.await.is_err());
    let decided = fixture.coordination.decide_approval_as(
        &**fixture,
        Some(&mastering),
        request.number,
        true,
        None,
        Default::default(),
        None,
    );
    assert_eq!(decided.await.unwrap().status, DecisionStatus::Approved);
    (fixture.coordination.authorize_action(
        &**fixture,
        Some(&Fixture::caller(&worker.id)),
        setup(),
    ))
    .await
    .unwrap();

    // A key it only proposes stays the user's to approve, below it too,
    // though it may deny.
    let restart = || Action::new(PermissionKey::Daemon, "shoal daemon restart");
    let below = undecided(
        fixture
            .coordination
            .authorize_action(&**fixture, Some(&Fixture::caller(&worker.id)), restart())
            .await,
    );
    let approving = fixture.coordination.decide_approval_as(
        &**fixture,
        Some(&mastering),
        below.number,
        true,
        None,
        Default::default(),
        None,
    );
    let refused = format!("{:#}", approving.await.unwrap_err());
    assert!(refused.contains("daemon"), "{refused}");
    let denied = fixture.coordination.decide_approval_as(
        &**fixture,
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
        fixture
            .coordination
            .authorize_action(&**fixture, Some(&mastering), own)
            .await,
    );
    assert!(
        !fixture
            .coordination
            .decides_as_user(&**fixture, &mastering, &own)
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn a_chat_without_a_repository_takes_an_action_as_its_repository_allows() {
    let fixture = Arc::new(Fixture::new().await);
    let repo = fixture.repository("repo").await;
    let config = "[modes.orchestrate.permissions]\nlimits = 'deny'\n";
    fixture.set_local_config(&repo, config);
    let master = chat(&fixture, "master", "orchestrate", None, None).await;
    let start = || "start the agent of worker now".to_owned();
    let mastering = Fixture::caller(&master.id);
    (fixture
        .coordination
        .authorize_beyond_limits(&**fixture, &mastering, start(), None))
    .await
    .unwrap();
    let denied = fixture.coordination.authorize_beyond_limits(
        &**fixture,
        &mastering,
        start(),
        Some(repo.clone()),
    );
    let error = denied.await.unwrap_err();
    assert!(error.downcast_ref::<Undecided>().is_none(), "{error:#}");
}

#[tokio::test]
async fn a_decider_without_a_repository_decides_as_the_action_s_repository_allows() {
    let fixture = Arc::new(Fixture::new().await);
    let repo = fixture.repository("repo").await;
    let config = "[modes.orchestrate.permissions]\nadmin = 'propose'\n";
    fixture.set_local_config(&repo, config);
    let master = chat(&fixture, "master", "orchestrate", None, None).await;
    let below = chat(&fixture, "below", "chat", None, Some(&master.id)).await;
    let setup = Action::new(PermissionKey::Admin, "shoal setup --skip 3").in_repository(repo);
    let request = undecided(
        fixture
            .coordination
            .authorize_action(&**fixture, Some(&Fixture::caller(&below.id)), setup)
            .await,
    );
    // The repository holds the master session's admin to propose, so it
    // may only deny or pass up what its global config would let it approve.
    let mastering = Fixture::caller(&master.id);
    let approving = fixture.coordination.decide_approval_as(
        &**fixture,
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
