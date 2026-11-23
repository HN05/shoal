use crate::stand_in::Fixture;

#[tokio::test]
async fn the_queue_admits_nothing_while_the_user_is_away() {
    let fixture = Fixture::new().await;
    let agents = &fixture.agents;
    let presence = agents.presence();
    agents.user_away(true);
    assert!(agents.agent_queue.holds_admission());
    assert!(presence.borrow().away);
    agents.user_away(false);
    assert!(!agents.agent_queue.holds_admission());
    assert!(!presence.borrow().away);
}
