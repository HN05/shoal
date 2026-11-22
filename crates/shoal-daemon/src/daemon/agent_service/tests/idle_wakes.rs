use std::time::Duration;

use shoal_agents::idle_wakes::Woken;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

use crate::{
    config::modes::ModeName,
    daemon::{
        manager::Manager,
        test_support::{chat, configure, manager, repository},
    },
    state::{AgentState, Sleep},
    time::UnixTime,
};

/// The unread messages of the space `id`.
async fn messages(manager: &Manager, id: &SpaceId) -> Vec<String> {
    (manager.agents)
        .agent_messages(manager, &SpaceSelector::id(id))
        .await
        .unwrap()
        .into_iter()
        .map(|message| message.message)
        .collect()
}

#[tokio::test]
async fn a_chat_sleeps_whatever_its_mode() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    configure(&manager, "[modes.plan]\nidle_wake_minutes = 30\n").await;
    let talks = chat(&manager, "talks", ModeName::CHAT, Some(&repo), None).await;
    let plans = chat(&manager, "plans", ModeName::PLAN, Some(&repo), None).await;
    let selector = SpaceSelector::id(&talks.id);
    let slept = (manager.agents)
        .sleep(&*manager, &selector, Some(Sleep::Permanent), None)
        .await
        .unwrap();
    assert_eq!(
        (slept.sleep, slept.idle_wake_minutes),
        (Some(Sleep::Permanent), None)
    );
    for chat in [&talks, &plans] {
        let idle = AgentState::Idle;
        (manager.agents)
            .set_agent_state(&*manager, &SpaceSelector::id(&chat.id), idle, None, None)
            .await
            .unwrap();
    }
    let later = UnixTime::now() + Duration::from_secs(3660);
    (manager.agents)
        .wake_idle_agents(&*manager, &mut Woken::default(), later)
        .await
        .unwrap();
    assert!(messages(&manager, &talks.id).await.is_empty());
    let wake = messages(&manager, &plans.id).await;
    assert!(wake[0].contains("idle for 30 minutes"), "{wake:?}");
}
