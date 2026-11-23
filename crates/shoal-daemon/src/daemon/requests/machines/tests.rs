use std::time::Duration;

use super::handle;
use crate::{
    daemon::{
        answer::Answer,
        scope::{Access, Caller, Scoped},
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
        access: Access::Full,
        vouched: None,
    };
    let mut pause = MachineMethod::PauseMachine { machine: None };
    assert!(pause.scope(&manager, &caller).await.is_err());
    let Answer::Body(Body::MachinePaused(paused)) = handle(&manager, pause).await.unwrap() else {
        panic!("not a pause's answer");
    };
    assert!(manager.agents.agent_queue().holds_admission());
    assert_eq!(manager.agents.paused_since(), Some(paused.since));
    // The unpause is refused until the pause's stop, in the background, ended.
    let unpaused = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let unpause = MachineMethod::UnpauseMachine { machine: None };
            if let Ok(Answer::Body(Body::MachineUnpaused(unpaused))) =
                handle(&manager, unpause).await
            {
                return unpaused;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
    let unpaused = unpaused.await.unwrap();
    assert_eq!(unpaused.paused_since, Some(paused.since));
    assert!(!manager.agents.agent_queue().holds_admission());
}
