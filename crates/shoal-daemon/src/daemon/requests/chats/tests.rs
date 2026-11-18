use std::collections::BTreeMap;

use super::*;
use crate::{
    config::{modes::ModeName, permissions::PermissionKey},
    daemon::{
        approvals::{Target, Undecided},
        test_support::{chat, global_config, manager, repository},
    },
    model::ChatQueueRequest,
    protocol::LaunchMethod,
};
use shoal_protocol::launches::{Launch, LaunchSource, SpaceChoice, StartTarget};

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

/// A machine whose one heavy slot the launch of a heavy chat `pair` holds,
/// with `claude` a program every runner has; and the start of a chat in
/// that mode in `repo`.
async fn full_machine(manager: &Manager, repo: &crate::model::Repository) -> ChatStart {
    manager.publish_config(global_config(
        "[machine]\nmax_heavy_agents = 1\n[machine.overload.disk]\nenabled = false\n\
         [modes.pair]\nkind = 'chat'\n[agents.claude]\ncommand = ['sh']\n",
    ));
    let start = ChatStart {
        repository: Some((&repo.id).into()),
        name: Some("talk".into()),
        agent: Some("claude".into()),
        mode: Some("pair".into()),
        ..ChatStart::default()
    };
    let holding = manager.queue_chat_add(pair_launch(), None).await;
    assert!(holding.unwrap().start);
    start
}

/// The launch of a heavy chat's agent.
fn pair_launch() -> ChatQueueRequest {
    ChatQueueRequest {
        source: None,
        repository: None,
        name: "talk".into(),
        agent: "claude".into(),
        mode: "pair".into(),
        narrowing: Default::default(),
        plan: "{}".into(),
        env: BTreeMap::new(),
        now: false,
        priority: None,
    }
}

/// A client without a terminal, as the user, records the chat its waiting
/// launch starts in, which the launch then holds.
#[tokio::test]
async fn an_unscoped_client_records_the_chat_of_its_waiting_launch() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let start = full_machine(&manager, &repo).await;
    let waiting = manager.queue_chat_add(pair_launch(), None).await.unwrap();
    let entry = waiting.entry.unwrap();
    assert!(!waiting.start);

    let launched = Launched {
        chat: None,
        origin: None,
        entry: Some(entry.id.clone()),
    };
    let launched = launched.take(&manager, start, None).await.unwrap();

    let entries = manager.queue_entries().await.unwrap();
    let queued = entries.iter().find(|queued| queued.id == entry.id).unwrap();
    assert_eq!(queued.space_id.as_ref(), Some(&launched.chat.id));
}

/// A scoped caller's chat start takes the slot only of a launch its scope
/// reaches, never of one the user queued.
#[tokio::test]
async fn a_scoped_chat_start_cannot_take_a_launch_outside_its_scope() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let start = full_machine(&manager, &repo).await;
    let users = manager.queue_chat_add(pair_launch(), None).await;
    let entry = users.unwrap().entry.unwrap();
    let starter = chat(&manager, "starter", "chat", Some(&repo), None).await;
    let caller = Caller {
        space_id: starter.id.clone(),
        execution: None,
        granted: None,
        read_only: false,
    };
    let mut method = ChatMethod::LaunchChat {
        start: Box::new(start),
        chat: None,
        origin: None,
        queue_entry: Some(entry.id.clone()),
    };

    let refused = method.scope(&manager, &caller).await.map(drop).unwrap_err();

    assert!(
        format!("{refused:#}").contains("outside this space's scope"),
        "{refused:#}"
    );
}

/// The process that runs a new chat's agent ends the chat's failed start in
/// the chat's own scope, while a chat outside the caller's scope is
/// refused.
/// A scoped caller that runs the global config's setup chat itself queues
/// its agent only as its `admin` permission allows without a repository.
#[tokio::test]
async fn a_scoped_setup_chat_it_runs_itself_asks_the_admin_permission() {
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
    let mut queued = ChatQueueRequest {
        source: None,
        repository: None,
        name: "setup".into(),
        agent: "claude".into(),
        mode: ModeName::SETUP,
        narrowing: Default::default(),
        plan: "{}".into(),
        env: BTreeMap::new(),
        now: false,
        priority: None,
    };
    let confined = manager.confine_chat_launch(&caller, &mut queued).await;
    match confined.unwrap_err().downcast::<Undecided>() {
        Ok(Undecided(request)) => assert_eq!(request.target, Target::Action(PermissionKey::Admin)),
        Err(error) => panic!("expected an approval request: {error:#}"),
    }
}
