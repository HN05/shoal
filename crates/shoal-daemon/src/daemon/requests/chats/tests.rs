use std::collections::BTreeMap;

use super::*;
use crate::{
    daemon::test_support::{chat, global_config, manager, repository},
    model::ChatQueueRequest,
};

/// A chat the user's process records for a launch a scoped process queued
/// goes below that process's space, as its own start would put it; one for
/// a scoped caller, whose own scope confines it, takes nothing from the
/// launch.
#[tokio::test]
async fn a_chat_recorded_for_a_scoped_launch_goes_below_its_queuer() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager.publish_config(global_config(
        "[machine]\nmax_agents = 1\n[machine.overload.disk]\nenabled = false\n",
    ));
    let queuer = chat(&manager, "queuer", "chat", Some(&repo), None).await;
    let caller = Caller {
        space_id: queuer.id.clone(),
        execution: None,
    };
    let launch = ChatQueueRequest {
        repository: Some((&repo.id).into()),
        name: "talk".into(),
        agent: "claude".into(),
        mode: "chat".into(),
        narrowing: Default::default(),
        plan: "{}".into(),
        env: BTreeMap::new(),
        now: true,
        priority: None,
        records_when_waiting: false,
    };
    let entry = manager.queue_chat_add(launch, Some(&caller)).await.unwrap();
    let entry = entry.entry.unwrap().id;

    let mut parent = None;
    let queued = queued_scope(&manager, None, Some(&entry), &mut parent).await;

    assert_eq!(
        queued.unwrap().map(|queued| queued.space_id),
        Some(queuer.id.clone())
    );
    assert_eq!(parent, Some(SpaceSelector::id(&queuer.id)));
    let mut own = None;
    let scoped = queued_scope(&manager, Some(&caller), Some(&entry), &mut own).await;
    assert!(scoped.unwrap().is_none() && own.is_none());
}
