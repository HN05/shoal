use std::sync::atomic::Ordering;

use shoal_daemon_base::jobs::JobStop;
use shoal_protocol::{
    hub::packages::{Handing, Leaving},
    space_names::SpaceSelector,
};

use crate::{stand_in::Fixture, state::AgentState};

#[tokio::test]
async fn a_pause_holds_admission_through_a_restart_until_unpaused() {
    let fixture = Fixture::new().await;
    let (agents, daemon) = (&fixture.agents, &*fixture.daemon);
    let (since, stop) = agents.pause().await.unwrap();
    // Until its stop ran, an unpause is refused.
    assert!(agents.unpause(daemon).await.is_err());
    agents.stop_for_pause(daemon, stop.unwrap()).await;
    assert!(agents.agent_queue.holds_admission());
    // Pausing again keeps since when it is paused.
    assert_eq!(agents.pause().await.unwrap().0, since);
    // A new daemon finds the pause in the store.
    agents.agent_queue.pause(None);
    agents.keep_pause().await.unwrap();
    assert_eq!(agents.paused_since(), Some(since));
    assert!(agents.agent_queue.holds_admission());
    assert_eq!(agents.unpause(daemon).await.unwrap(), Some(since));
    assert!(!agents.agent_queue.holds_admission());
    assert_eq!(daemon.stays.load(Ordering::SeqCst), 1);
    // Unpausing a machine that is not paused retires nothing.
    assert_eq!(agents.unpause(daemon).await.unwrap(), None);
    assert_eq!(daemon.stays.load(Ordering::SeqCst), 1);
    agents.keep_pause().await.unwrap();
    assert!(!agents.agent_queue.holds_admission());
}

#[tokio::test]
async fn a_pause_hands_over_the_spaces_with_work_and_ends_ci_jobs() {
    let fixture = Fixture::new().await;
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
    let (_, stop) = fixture.agents.pause().await.unwrap();
    (fixture.agents)
        .stop_for_pause(&*fixture.daemon, stop.unwrap())
        .await;
    let daemon = &fixture.daemon;
    assert_eq!(
        *daemon.handings.lock().unwrap(),
        [Handing::Only([working.id].into())]
    );
    assert_eq!(*daemon.leavings.lock().unwrap(), [Leaving::Paused]);
    let pause = JobStop::Pause.to_string();
    assert_eq!(*daemon.job_stops.lock().unwrap(), [pause]);
}

#[tokio::test]
async fn an_unpause_is_refused_until_the_pause_s_hand_over_ends() {
    let fixture = std::sync::Arc::new(Fixture::new().await);
    let sending = fixture.daemon.handing_gate.write().await;
    let (_, stop) = fixture.agents.pause().await.unwrap();
    let stopping = tokio::spawn({
        let fixture = fixture.clone();
        async move {
            let stop = stop.unwrap();
            fixture.agents.stop_for_pause(&*fixture.daemon, stop).await
        }
    });
    fixture.daemon.handing_started.notified().await;
    assert!(fixture.agents.unpause(&*fixture.daemon).await.is_err());
    // A pause meanwhile leaves the stop to the one under way.
    assert!(fixture.agents.pause().await.unwrap().1.is_none());
    assert!(fixture.agents.agent_queue.holds_admission());
    drop(sending);
    stopping.await.unwrap();
    assert!(
        fixture
            .agents
            .unpause(&*fixture.daemon)
            .await
            .unwrap()
            .is_some()
    );
}
