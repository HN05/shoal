use crate::stand_in::Fixture;

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
async fn a_space_has_something_to_do_while_its_agent_works() {
    use crate::state::AgentState;
    use shoal_protocol::space_names::SpaceSelector;
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
    let spaces = fixture.agents.spaces_with_work().await.unwrap();
    assert_eq!(spaces, [working.id].into());
}
