use crate::{
    daemon::test_support::{chat, global_config, manager, repository},
    protocol::ExecutionMethod,
};
use shoal_protocol::{
    executions::ExecutionKind, launches::LaunchSource, space_names::SpaceSelector,
};

/// The request of a wrapper in this process to restore an agent in
/// `space`, which `source` made.
fn restore(space: &SpaceSelector, source: Option<LaunchSource>) -> ExecutionMethod {
    ExecutionMethod::Execute {
        space: space.clone(),
        wrapper: crate::process::identity::capture(std::process::id())
            .unwrap()
            .unwrap(),
        kind: ExecutionKind::Command,
        agent: Some("claude".into()),
        model: Default::default(),
        mode: None,
        narrowing: Default::default(),
        new_session: false,
        recover: true,
        source,
        reattach: true,
        queue_entry: None,
        demo: None,
        origin: None,
        merge: None,
    }
}

/// A restore the user runs in their own terminal, as `restore --focus`
/// does, is their interaction with its agent; a wrapper's restore that
/// names no source is not.
#[tokio::test]
async fn a_restore_the_user_runs_here_engages_them() {
    let (temp, manager) = manager().await;
    manager.publish_config(global_config("[agents.claude]\ncommand = ['sh']\n"));
    let path = repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None);
    let repo = repo.await.unwrap();
    let focused = chat(&manager, "focused", "chat", Some(&repo), None).await;
    let other = chat(&manager, "other", "chat", Some(&repo), None).await;
    let (focused, other) = (focused.id, other.id);

    let manual = Some(LaunchSource::Manual);
    super::handle(
        &manager,
        restore(&SpaceSelector::id(&focused), manual),
        None,
    )
    .await
    .map(drop)
    .unwrap();
    super::handle(&manager, restore(&SpaceSelector::id(&other), None), None)
        .await
        .map(drop)
        .unwrap();

    let interactions = manager.agents.interactions().await.unwrap();
    assert!(interactions.contains_key(&focused));
    assert!(!interactions.contains_key(&other));
}
