use super::*;
use crate::daemon::workspace_files::Attacher;
use crate::forge::items::link::Selection;
use crate::test_support::{manager, repository};

fn sent_at(times: &[i64]) -> Vec<AgentMessage> {
    times
        .iter()
        .enumerate()
        .map(|(n, &created_at)| AgentMessage {
            id: i64::try_from(n).unwrap(),
            created_at,
            message: format!("message {n}"),
            attachments: Vec::new(),
        })
        .collect()
}

#[test]
fn a_run_settles_once_none_arrives_for_the_window() {
    let run = sent_at(&[100, 103]);
    assert_eq!(held_until(&run, 5, 103), Some(108));
    assert_eq!(held_until(&run, 5, 107), Some(108));
    assert_eq!(held_until(&run, 5, 108), None);
    // Without a window, each message is delivered at once.
    assert_eq!(held_until(&run, 0, 103), None);
    assert_eq!(held_until(&[], 5, 100), None);
}

#[test]
fn a_steady_run_settles_at_most_six_windows_after_its_first_message() {
    let run = sent_at(&[100, 104, 108, 112, 116, 120, 124, 128]);
    assert_eq!(held_until(&run, 5, 128), Some(130));
    assert_eq!(held_until(&run, 5, 130), None);
}

/// A workspace whose repository holds messages for `settle` seconds.
async fn settling_workspace(temp: &tempfile::TempDir, manager: &Manager, settle: u64) -> Workspace {
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let config = format!("[messages]\nsettle_seconds = {settle}\n");
    manager
        .write_local_config(&repo.id, Some(config))
        .await
        .unwrap();
    manager
        .create_workspace(&repo.id, "receiver".into(), None, None, None)
        .await
        .unwrap()
}

/// Move the workspace's messages `seconds` into the past.
async fn age_messages(manager: &Manager, workspace: &Workspace, seconds: i64) {
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE agent_messages SET created_at=created_at-?2 WHERE workspace_id=?1",
                rusqlite::params![id, seconds],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

/// Return once a wake prompt is due for the workspace's agent.
async fn wake_due(manager: &Manager, workspace: &Workspace) {
    // A hang guard only: the wake settles within two seconds.
    let wait = manager.wait_agent_wake(&workspace.id, true, None);
    tokio::time::timeout(std::time::Duration::from_secs(60), wait)
        .await
        .expect("the wake was due")
        .unwrap();
}

async fn idle_agent(manager: &Manager, workspace: &Workspace) {
    manager
        .set_agent_state(&workspace.id, crate::state::AgentState::Idle, None, None)
        .await
        .unwrap();
}

async fn send(manager: &Manager, workspace: &Workspace, messages: &[&str]) {
    for &message in messages {
        manager
            .send_agent_message(&workspace.id, message.into(), Vec::new(), &Attacher::User)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn hooks_and_wake_prompts_deliver_a_run_once_it_settles() {
    let (temp, manager) = manager().await;
    let workspace = settling_workspace(&temp, &manager, 60).await;
    idle_agent(&manager, &workspace).await;
    send(
        &manager,
        &workspace,
        &["Request 1 approved", "Request 2 approved"],
    )
    .await;
    let delivered = manager.agent_updates(&workspace.id).await.unwrap();
    assert!(delivered.messages.is_empty(), "{delivered:?}");
    // `shoal messages` reads a run before it settles.
    assert_eq!(
        manager.agent_messages(&workspace.id).await.unwrap().len(),
        2
    );

    age_messages(&manager, &workspace, 60).await;
    wake_due(&manager, &workspace).await;
    let delivered = manager.agent_updates(&workspace.id).await.unwrap();
    let messages = delivered.messages.iter().map(|message| &message.message);
    assert_eq!(
        messages.collect::<Vec<_>>(),
        ["Request 1 approved", "Request 2 approved"]
    );
}

#[tokio::test]
async fn a_wait_returns_a_run_once_it_settles() {
    let (temp, manager) = manager().await;
    let workspace = settling_workspace(&temp, &manager, 60).await;
    send(
        &manager,
        &workspace,
        &["Request 1 approved", "Request 2 approved"],
    )
    .await;
    // Leave the run a moment to settle, so the wait holds it.
    age_messages(&manager, &workspace, 59).await;
    // A hang guard only: the run settles within two seconds.
    let selection = Selection::default();
    let wait = manager.wait_items(&workspace.id, &selection, None);
    let updates = tokio::time::timeout(std::time::Duration::from_secs(60), wait)
        .await
        .expect("the run settled")
        .unwrap();
    assert_eq!(updates.messages.len(), 2, "{updates:?}");
}

#[tokio::test]
async fn a_wake_prompt_follows_a_run_that_settles_while_it_waits() {
    let (temp, manager) = manager().await;
    let workspace = settling_workspace(&temp, &manager, 60).await;
    idle_agent(&manager, &workspace).await;
    send(&manager, &workspace, &["Request 1 approved"]).await;
    // The run settles within two seconds, announced by no signal.
    age_messages(&manager, &workspace, 59).await;
    wake_due(&manager, &workspace).await;
}

#[tokio::test]
async fn a_wake_prompt_does_not_wait_for_the_pr_gate() {
    let (temp, manager) = manager().await;
    let workspace = settling_workspace(&temp, &manager, 60).await;
    idle_agent(&manager, &workspace).await;
    send(&manager, &workspace, &["Request 1 approved"]).await;
    age_messages(&manager, &workspace, 60).await;
    // Cleanup or an item change holds the gate for minutes.
    let _held = manager.gates.lock_prs([&workspace.id]).await;
    wake_due(&manager, &workspace).await;
}
