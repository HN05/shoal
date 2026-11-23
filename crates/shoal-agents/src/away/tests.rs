use std::{sync::atomic::Ordering, time::Duration};

use shoal_protocol::{
    hub::{
        packages::{Handed, Handing},
        placement::Decision,
    },
    model::{Space, Workspace},
    space_names::SpaceSelector,
};
use shoal_schedule::placement::{Policy, Reason};

use crate::{stand_in::Fixture, state::AgentState};

/// A fixture under `keep_awake = "handover"` on a machine joined to a hub,
/// with a workspace whose agent works and one whose agent is idle.
async fn handing_over() -> (Fixture, Workspace, Workspace) {
    let fixture = Fixture::with_config("[machine]\nkeep_awake = 'handover'\n").await;
    fixture.daemon.hands_over.store(true, Ordering::SeqCst);
    let (working, idle) = (
        fixture.workspace("working").await,
        fixture.workspace("idle").await,
    );
    for (workspace, state) in [(&working, AgentState::Working), (&idle, AgentState::Idle)] {
        let space = SpaceSelector::id(&workspace.id);
        (fixture.agents)
            .set_agent_state(&*fixture.daemon, &space, state, None, None)
            .await
            .unwrap();
    }
    (fixture, working, idle)
}

/// The hub's word that `workspace` went to another machine.
fn went(workspace: &Workspace) -> (Space, Handed) {
    let decision = Decision {
        machine: "desktop".into(),
        name: "desktop".into(),
        reason: Reason::Caller,
        policy: Policy::Balanced,
        unanswered: None,
        skipped: Vec::new(),
    };
    (Space::from(workspace.clone()), Handed::Went(decision))
}

fn handings(fixture: &Fixture) -> Vec<Handing> {
    fixture.daemon.handings.lock().unwrap().clone()
}

#[tokio::test]
async fn the_queue_admits_nothing_while_the_user_is_away() {
    let fixture = Fixture::new().await;
    let agents = &fixture.agents;
    let presence = agents.presence();
    agents.user_away(&*fixture.daemon, true).await;
    assert!(agents.agent_queue.holds_admission());
    assert!(presence.borrow().away);
    agents.user_away(&*fixture.daemon, false).await;
    assert!(!agents.agent_queue.holds_admission());
    assert!(!presence.borrow().away);
}

#[tokio::test]
async fn a_space_has_something_to_do_while_its_agent_works_or_waits_to_be_restored() {
    let (fixture, working, idle) = handing_over().await;
    let launching = fixture.workspace("launching").await;
    let (idle_id, launching_id) = (idle.id.clone(), launching.id.clone());
    (fixture.agents.store)
        .run(move |db| {
            // A launch, whose agent never ran, has nothing to carry.
            db.execute(
                "INSERT INTO agent_queue(id,kind,repository_id,name,agent,plan,env,
                     execution_id,state,created_at,seen_at,space_id) VALUES
                 ('restore','restore','repo','restore','codex',NULL,'{}','execution','queued',0,99,?1),
                 ('launch','launch','repo','launch','codex','{}','{}',NULL,'queued',0,99,?2)",
                rusqlite::params![idle_id, launching_id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let spaces = fixture.agents.spaces_with_work().await.unwrap();
    assert_eq!(spaces, [working.id, idle.id].into());
}

#[tokio::test]
async fn walking_away_hands_the_spaces_with_work_over_once_until_the_user_is_back() {
    let (fixture, working, _) = handing_over().await;
    let (agents, daemon) = (&fixture.agents, &*fixture.daemon);
    agents.user_away(daemon, true).await;
    agents.user_away(daemon, true).await;
    assert_eq!(
        handings(&fixture),
        [Handing::Only([working.id.clone()].into())]
    );
    assert!(!agents.presence().borrow().handing_over);
    assert!(agents.agent_queue.holds_admission());
    assert_eq!(daemon.stays.load(Ordering::SeqCst), 0);
    // Back, the machine admits work and retires what went.
    agents.user_away(daemon, false).await;
    assert!(!agents.agent_queue.holds_admission());
    assert_eq!(daemon.stays.load(Ordering::SeqCst), 1);
    // The next absence hands over again.
    agents.user_away(daemon, true).await;
    assert_eq!(handings(&fixture).len(), 2);
}

#[tokio::test]
async fn a_shutdown_after_walking_away_hands_over_every_space_but_those_that_went() {
    let (fixture, working, _) = handing_over().await;
    let (agents, daemon) = (&fixture.agents, &*fixture.daemon);
    daemon.handed.lock().unwrap().push(went(&working));
    agents.user_away(daemon, true).await;
    (agents.stop_for_system_shutdown(daemon, Duration::from_secs(60))).await;
    let gone = [working.id.clone()].into();
    assert_eq!(handings(&fixture)[1], Handing::AllBut(gone));
}

#[tokio::test]
async fn a_machine_that_hands_nothing_over_only_holds_its_queue() {
    let (fixture, _, _) = handing_over().await;
    fixture.daemon.hands_over.store(false, Ordering::SeqCst);
    fixture.agents.user_away(&*fixture.daemon, true).await;
    assert!(handings(&fixture).is_empty());
    assert!(fixture.agents.agent_queue.holds_admission());
}

#[tokio::test]
async fn a_sleep_does_not_wait_for_a_hand_over_under_way() {
    let (fixture, _, _) = handing_over().await;
    let under_way = fixture.agents.presences.handed_over.lock().await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    (fixture.agents)
        .hand_over_before_sleep(&*fixture.daemon, deadline)
        .await;
    drop(under_way);
    assert!(handings(&fixture).is_empty());
}

#[tokio::test]
async fn a_shutdown_during_a_walk_away_hand_over_leaves_it_the_spaces_under_way() {
    let (fixture, working, _) = handing_over().await;
    let sending = fixture.daemon.handing_gate.write().await;
    let walking_away = tokio::spawn({
        let (agents, daemon) = (fixture.agents.clone(), fixture.daemon.clone());
        async move { agents.user_away(&*daemon, true).await }
    });
    fixture.daemon.handing_started.notified().await;
    // The shutdown's own hand-over waits at the gate too, so it gets there
    // only if it did not wait for the first.
    let shutdown = tokio::spawn({
        let (agents, daemon) = (fixture.agents.clone(), fixture.daemon.clone());
        async move {
            let window = Duration::from_secs(60);
            agents.stop_for_system_shutdown(&*daemon, window).await
        }
    });
    fixture.daemon.handing_started.notified().await;
    drop(sending);
    shutdown.await.unwrap();
    walking_away.await.unwrap();
    let under_way = [working.id.clone()].into();
    assert_eq!(handings(&fixture)[1], Handing::AllBut(under_way));
}

#[tokio::test]
async fn coming_back_after_a_shutdown_stopped_work_keeps_its_hold() {
    let (fixture, _, _) = handing_over().await;
    let (agents, daemon) = (&fixture.agents, &*fixture.daemon);
    agents.user_away(daemon, true).await;
    (agents.stop_for_system_shutdown(daemon, Duration::from_secs(60))).await;
    agents.user_away(daemon, false).await;
    assert!(agents.agent_queue.holds_admission());
    assert_eq!(daemon.stays.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_working_chat_takes_the_workspace_whose_worktree_it_runs_in() {
    let (fixture, working, idle) = handing_over().await;
    let chat: Space = serde_json::from_value(serde_json::json!({
        "id": "chat",
        "name": "talk",
        "repository_id": "repo",
        "workdir": {
            "kind": "worktree",
            "workspace": {"id": idle.id, "name": "idle", "branch": "idle"},
            "path": "/work",
        },
        "kind": "chat",
        "started_at": 1,
    }))
    .unwrap();
    fixture.daemon.spaces.lock().unwrap().push(chat);
    (fixture.agents.store)
        .run(|db| {
            db.execute(
                "INSERT INTO space_agent_state(space_id,state,since) VALUES ('chat','working',1)",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let spaces = (fixture.agents.spaces_to_hand_over(&*fixture.daemon))
        .await
        .unwrap();
    let chat: shoal_ids::space_ids::SpaceId = "chat".into();
    assert_eq!(spaces, [chat, idle.id, working.id].into());
}
