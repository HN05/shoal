use super::handle;
use crate::{
    daemon::{
        answer::Answer,
        scope::{Caller, Scoped},
        test_support::{chat, manager},
    },
    protocol::{Body, MachineMethod},
};

#[tokio::test]
async fn only_the_user_pauses_a_machine_which_admits_nothing_until_unpaused() {
    let (_temp, manager) = manager().await;
    let space = chat(&manager, "talk", "worker", None, None).await;
    let caller = Caller {
        space_id: space.id.clone(),
        execution: None,
        granted: None,
        read_only: false,
        vouched: None,
    };
    let mut pause = MachineMethod::PauseMachine { machine: None };
    assert!(pause.scope(&manager, &caller).await.is_err());
    let Answer::Body(Body::MachinePaused(paused)) = handle(&manager, pause).await.unwrap() else {
        panic!("not a pause's answer");
    };
    assert!(manager.agents.agent_queue().holds_admission());
    assert_eq!(manager.agents.paused_since(), Some(paused.since));
    let unpause = MachineMethod::UnpauseMachine { machine: None };
    let Answer::Body(Body::MachineUnpaused(unpaused)) = handle(&manager, unpause).await.unwrap()
    else {
        panic!("not an unpause's answer");
    };
    assert_eq!(unpaused.paused_since, Some(paused.since));
    assert!(!manager.agents.agent_queue().holds_admission());
}
