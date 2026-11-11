use crate::client::space_ids::SpaceId;
use std::sync::Mutex;

/// The workspaces whose last session a relocation continued.
pub(crate) static RESUMED: Mutex<Vec<SpaceId>> = Mutex::new(Vec::new());

use crate::client::space_names::SpaceSelector;
use crate::{
    config::modes::ModeName,
    daemon::{
        conversions::Converting,
        test_support::{ProcessGroup, manager, repository},
        workspace::Manager,
    },
    execution::recovery::{LastSession, LastSessionExt},
    model::Workspace,
    state::AgentState,
};

/// A chat of a new repository, whose agent `process` reported session
/// `s1`, converted into a workspace on `fix-login`.
async fn converted(
    root: &std::path::Path,
    manager: &Manager,
) -> (
    Workspace,
    ProcessGroup,
    crate::client::scope_tokens::ScopeToken,
) {
    let path = repository(root, "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let (talk, process) =
        crate::daemon::test_support::chat(manager, "talk", "worker", Some(&repo), None).await;
    let selector = SpaceSelector::id(&talk.id);
    let working = AgentState::Working;
    (manager.set_agent_state(&selector, working, None, Some("s1".into())))
        .await
        .unwrap();
    let talk = manager.chat_by_id(&talk.id).await.unwrap().unwrap();
    let token = manager.chat_scope(&talk.id).await.unwrap();
    let converting = Converting {
        chat: talk,
        mode: ModeName::PLAN,
    };
    let workspace = manager
        .create_converted_workspace(converting, "fix-login".into(), None, Default::default())
        .await
        .unwrap();
    (workspace, process, token)
}

#[tokio::test]
async fn the_agent_restarts_in_the_worktree_once_its_turn_ends() {
    let (root, manager) = manager().await;
    let (workspace, mut process, token) = converted(root.path(), &manager).await;
    let selector = SpaceSelector::id(&workspace.id);
    // The agent that converted its chat goes on in its turn, under its scope.
    assert!(manager.caller(&token).await.unwrap().is_some());

    let idle = AgentState::Idle;
    (manager.set_agent_state(&selector, idle, None, None))
        .await
        .unwrap();
    manager.relocate_finished_turns().await.unwrap();

    process.wait().unwrap();
    assert_eq!(
        *RESUMED.lock().unwrap(),
        std::slice::from_ref(&workspace.id)
    );
    let sessions = LastSession::all(&manager.paths, &workspace.id).unwrap();
    let [last] = sessions.as_slice() else {
        panic!("one last session to continue: {sessions:?}");
    };
    assert!(last.converted);
    assert_eq!(
        last.session.as_ref().map(ToString::to_string),
        Some("s1".into())
    );
    assert_eq!(last.mode, ModeName::PLAN);
    assert!(manager.caller(&token).await.unwrap().is_none());
    let id = workspace.id.clone();
    let pending = (manager.store)
        .run(move |db| crate::daemon::conversions::pending(db, &id))
        .await
        .unwrap();
    assert!(pending.is_none());
}

#[tokio::test]
async fn a_working_agent_finishes_its_turn_where_it_runs() {
    let (root, manager) = manager().await;
    let (workspace, _process, token) = converted(root.path(), &manager).await;

    manager.relocate_finished_turns().await.unwrap();

    assert!(RESUMED.lock().unwrap().is_empty());
    assert!(manager.caller(&token).await.unwrap().is_some());
    let sessions = LastSession::all(&manager.paths, &workspace.id).unwrap();
    assert!(sessions.is_empty(), "{sessions:?}");
}

#[tokio::test]
async fn an_agent_waits_where_it_runs_while_the_workspaces_setup_failed() {
    let (root, manager) = manager().await;
    let (workspace, _process, token) = converted(root.path(), &manager).await;
    let id = workspace.id.clone();
    (manager.store)
        .run(move |db| {
            db.execute("UPDATE workspaces SET state='failed' WHERE id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();
    let selector = SpaceSelector::id(&workspace.id);
    (manager.set_agent_state(&selector, AgentState::Idle, None, None))
        .await
        .unwrap();

    manager.relocate_finished_turns().await.unwrap();

    assert!(RESUMED.lock().unwrap().is_empty());
    assert!(manager.caller(&token).await.unwrap().is_some());
}

#[tokio::test]
async fn a_stop_of_the_workspace_keeps_the_converting_agents_session() {
    let (root, manager) = manager().await;
    let (workspace, mut process, token) = converted(root.path(), &manager).await;
    let targets = (manager.stop_targets(crate::protocol::QueuedLaunches::Keep))
        .await
        .unwrap();
    assert!(targets.iter().any(|target| target.id == workspace.id));

    let selector = SpaceSelector::id(&workspace.id);
    let keep = crate::protocol::QueuedLaunches::Keep;
    (manager.stop_workspace_manually(&selector, false, None, keep))
        .await
        .unwrap();

    process.wait().unwrap();
    assert!(RESUMED.lock().unwrap().is_empty());
    let sessions = LastSession::all(&manager.paths, &workspace.id).unwrap();
    let [last] = sessions.as_slice() else {
        panic!("one last session to continue: {sessions:?}");
    };
    assert!(last.converted);
    assert!(manager.caller(&token).await.unwrap().is_none());
}

#[tokio::test]
async fn the_relocation_restarts_no_agent_a_stop_handed_over() {
    let (root, manager) = manager().await;
    let (workspace, _process, _token) = converted(root.path(), &manager).await;
    let selector = SpaceSelector::id(&workspace.id);
    (manager.set_agent_state(&selector, AgentState::Idle, None, None))
        .await
        .unwrap();
    let keep = crate::protocol::QueuedLaunches::Keep;
    (manager.stop_workspace_manually(&selector, false, None, keep))
        .await
        .unwrap();

    manager.relocate_finished_turns().await.unwrap();

    assert!(RESUMED.lock().unwrap().is_empty());
}
