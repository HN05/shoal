use shoal_protocol::space_names::SpaceSelector;

use super::*;
use crate::stand_in::Fixture;

async fn has_work(fixture: &Fixture) -> bool {
    has(fixture, Work::TurnsAndWatches).await
}

async fn has(fixture: &Fixture, work: Work) -> bool {
    fixture.agents.agents_have_work(work).await.unwrap()
}

async fn report(fixture: &Fixture, id: &SpaceId, state: AgentState) {
    let space = SpaceSelector::id(id);
    (fixture
        .agents
        .set_agent_state(&*fixture.daemon, &space, state, None, None))
    .await
    .unwrap();
}

#[tokio::test]
async fn a_working_agent_has_work() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace("agent").await;
    assert!(!has_work(&fixture).await);
    for (state, work) in [(AgentState::Working, true), (AgentState::Idle, false)] {
        report(&fixture, &workspace.id, state).await;
        assert_eq!(has_work(&fixture).await, work, "{state}");
    }
}

#[tokio::test]
async fn an_idle_agent_has_work_while_it_receives_an_open_prs_activity() {
    let fixture = Fixture::new().await;
    let workspace = fixture.workspace("agent").await;
    fixture
        .records
        .open_prs
        .lock()
        .unwrap()
        .insert(workspace.id.clone());
    report(&fixture, &workspace.id, AgentState::Idle).await;
    assert!(has_work(&fixture).await);
    // Once the user walked away under `finish`, only a working turn holds.
    assert!(!has(&fixture, Work::Turns).await);
    report(&fixture, &workspace.id, AgentState::Working).await;
    assert!(has(&fixture, Work::Turns).await);
    // An agent waiting for the user cannot take the PR's activity.
    report(&fixture, &workspace.id, AgentState::Waiting).await;
    assert!(!has_work(&fixture).await);
}

#[tokio::test]
async fn a_failed_inhibitor_is_retried_under_another_config() {
    let fixture = Fixture::new().await;
    // `cat` holds, reading the input the daemon keeps open.
    let holds: &[&str] = &["sh", "-c", "$0"];
    let config = fixture.agents.config.current();
    let hold = Hold::Failed(config.clone());
    let hold = update(&*fixture.daemon, hold, true, &config, holds).await;
    assert!(matches!(hold, Hold::Failed(_)));
    let reloaded = Arc::new(Config::default());
    let hold = update(&*fixture.daemon, hold, true, &reloaded, holds).await;
    assert!(matches!(hold, Hold::Held(_)));
    let hold = update(&*fixture.daemon, hold, false, &reloaded, holds).await;
    assert!(matches!(hold, Hold::Released));
    // A failure to start notifies and waits for the next config.
    let missing: &[&str] = &["/nonexistent/shoal-inhibitor"];
    let hold = update(&*fixture.daemon, hold, true, &reloaded, missing).await;
    assert!(matches!(hold, Hold::Failed(ref failed) if Arc::ptr_eq(failed, &reloaded)));
    let notified = fixture.daemon.notified.lock().unwrap();
    assert_eq!(notified[0].0, AlertKind::KeepAwakeFailed);
}

#[tokio::test]
async fn an_inhibitor_lasts_until_its_input_closes() {
    // `cat` reads the input the daemon holds, then the script reports its end.
    let mut inhibitor = Inhibitor::start(&["sh", "-c", "$0; echo released >&2"]).unwrap();
    assert!(inhibitor.child.try_wait().unwrap().is_none());
    drop(inhibitor._input.take());
    let ended = inhibitor.ended().await.to_string();
    assert!(ended.contains("released"), "{ended}");
}

#[tokio::test]
async fn a_failed_inhibitor_reports_why() {
    let mut inhibitor = Inhibitor::start(&["sh", "-c", "echo denied >&2; exit 1"]).unwrap();
    let ended = inhibitor.ended().await.to_string();
    assert!(ended.contains("denied"), "{ended}");
    let missing = Inhibitor::start(&["/nonexistent/shoal-inhibitor"])
        .err()
        .unwrap();
    assert!(
        format!("{missing:#}").contains("/nonexistent/shoal-inhibitor"),
        "{missing:#}"
    );
}

#[tokio::test]
async fn an_inhibitor_writing_past_the_pipe_is_drained_and_bounded() {
    // More than a pipe holds: an undrained inhibitor would block before exiting.
    let script = "yes denied | head -c 1000000 >&2; exit 3";
    let mut inhibitor = Inhibitor::start(&["sh", "-c", script]).unwrap();
    let ended = tokio::time::timeout(Duration::from_secs(60), inhibitor.ended())
        .await
        .expect("the inhibitor's exit is seen")
        .to_string();
    assert!(
        ended.len() < shoal_subprocess::MAX_DIAGNOSTIC_CHARS + 100,
        "{}",
        ended.len()
    );
}
