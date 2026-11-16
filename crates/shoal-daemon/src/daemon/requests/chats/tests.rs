use std::collections::BTreeMap;

use super::*;
use crate::{
    config::permissions::PermissionKey,
    daemon::{
        approvals::{Target, Undecided},
        test_support::{chat, global_config, manager, repository},
    },
    model::ChatQueueRequest,
    protocol::LaunchMethod,
};
use shoal_protocol::launches::{Launch, LaunchSource, SpaceChoice, StartTarget};

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
        granted: None,
        read_only: false,
    };
    let launch = ChatQueueRequest {
        source: None,
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

/// A scoped caller starts a new chat's agent regardless of the limits only
/// as its `limits` permission allows, whether the daemon starts the chat or
/// queues its launch.
#[tokio::test]
async fn a_scoped_chat_start_now_asks_the_limits_permission() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let starter = chat(&manager, "starter", "chat", Some(&repo), None).await;
    let caller = Caller {
        space_id: starter.id.clone(),
        execution: None,
        granted: None,
        read_only: false,
    };
    let limits = |result: Result<()>| match result.unwrap_err().downcast::<Undecided>() {
        Ok(Undecided(request)) => request.target,
        Err(error) => panic!("expected an approval request: {error:#}"),
    };

    let launch = Launch {
        target: Some(StartTarget::Repository(repo.id.to_string())),
        chat: true,
        agent: Some("claude".into()),
        now: true,
        ..Launch::default()
    };
    let launched = (manager.launch(launch, LaunchSource::Manual, Some(&caller)))
        .await
        .map(drop);
    assert_eq!(limits(launched), Target::Action(PermissionKey::Limits));

    let mut queued = ChatQueueRequest {
        source: None,
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
    let confined = manager.confine_chat_launch(&caller, &mut queued).await;
    assert_eq!(limits(confined), Target::Action(PermissionKey::Limits));
}

/// A scoped `--now` start in a workspace outside the caller's scope is
/// refused before it asks the `limits` permission, so it makes no approval
/// request.
#[tokio::test]
async fn a_scoped_chat_start_now_elsewhere_is_refused_without_asking() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let starter = chat(&manager, "starter", "chat", Some(&repo), None).await;
    let other = (manager.create_workspace(&repo.id, "other".into(), None, None, None))
        .await
        .unwrap();
    let caller = Caller {
        space_id: starter.id.clone(),
        execution: None,
        granted: None,
        read_only: false,
    };

    let mut launch = LaunchMethod::Launch {
        launch: Box::new(Launch {
            in_workspace: Some(SpaceChoice::Space(SpaceSelector::id(&other.id))),
            chat: true,
            now: true,
            ..Launch::default()
        }),
    };
    // Its scope confines the workspace before the launch asks anything.
    let target = launch.scope(&manager, &caller).await.unwrap().unwrap();
    let error = (crate::daemon::scope::confine_target(&manager, &caller, target, false))
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<Undecided>().is_none(), "{error:#}");
}
