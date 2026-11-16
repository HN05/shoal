use super::*;
use crate::daemon::links::issue::test_support::{AGENT_TOKEN, Fixture, run_agent};
use crate::daemon::{workspace::ExecutionKind, workspace_files::Attacher};
use crate::execution::recovery::RecordExt;
use crate::paths::StateFiles;
use crate::removal::{InspectionPolicy, UnsavedWork};
use crate::{execution::TrackedAgent, model::Workspace};
use serde_json::json;
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::assignments::Done;
use shoal_protocol::executions::Stage;

/// A fixture whose agent account has a token, under `config`.
async fn agents(names: &[&str], assignees: &[&str], config: &str) -> Fixture {
    Fixture::start(names, assignees, config, AGENT_TOKEN).await
}

async fn remove(fixture: &Fixture, workspace: &Workspace) {
    let selector = SpaceSelector::id(&workspace.id);
    (fixture.manager)
        .remove_selected(&selector, UnsavedWork::default(), InspectionPolicy::GitOnly)
        .await
        .unwrap();
}

#[tokio::test]
async fn an_agents_assignment_puts_the_agent_account_on() {
    let fixture = agents(&["fix"], &["ann"], "").await;
    let warnings = fixture
        .link_as(&fixture.workspaces[0], Assigner::Agent)
        .await;
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("ann"), "{warnings:?}");
    assert_eq!(fixture.changes(), [json!(["ann", "shoal-agent"])]);
    assert_eq!(fixture.change_tokens(), ["token agent"]);
}

#[tokio::test]
async fn the_users_assignment_names_the_other_assignees_before_an_agent_works_it() {
    let fixture = agents(&["fix"], &["ann"], "").await;
    let warnings = fixture.link(&fixture.workspaces[0]).await;
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("ann"), "{warnings:?}");
    assert!(fixture.changes().is_empty());
}

#[tokio::test]
async fn the_user_hears_nothing_of_their_own_assignment() {
    let fixture = agents(&["fix"], &["shoal-user"], "").await;
    let warnings = fixture
        .link_as(&fixture.workspaces[0], Assigner::Agent)
        .await;
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(fixture.changes(), [json!(["shoal-user", "shoal-agent"])]);
}

#[tokio::test]
async fn the_users_assignment_to_a_space_without_an_agent_assigns_nobody() {
    let fixture = agents(&["fix"], &[], "").await;
    assert!(fixture.link(&fixture.workspaces[0]).await.is_empty());
    assert!(fixture.changes().is_empty());
}

#[tokio::test]
async fn the_agent_account_stays_while_another_agent_works_the_issue() {
    let fixture = agents(&["first", "second"], &[], "").await;
    let [first, second] = &fixture.workspaces[..] else {
        unreachable!()
    };
    fixture.link_as(first, Assigner::Agent).await;
    fixture.link_as(second, Assigner::Agent).await;
    assert!(fixture.unlink(first).await.is_empty());
    assert_eq!(fixture.changes(), [json!(["shoal-agent"])]);
    // Removing the last space whose agent works it takes it off.
    remove(&fixture, second).await;
    assert_eq!(fixture.changes(), [json!(["shoal-agent"]), json!([])]);
}

#[tokio::test]
async fn the_agent_account_another_shoal_put_on_stays_once_no_agent_here_works_the_issue() {
    let fixture = agents(&["fix"], &["shoal-agent"], "").await;
    let workspace = &fixture.workspaces[0];
    assert!(fixture.link(workspace).await.is_empty());
    remove(&fixture, workspace).await;
    assert!(fixture.changes().is_empty());
}

#[tokio::test]
async fn the_agent_account_another_shoal_took_off_goes_back_on() {
    let fixture = agents(&["fix"], &[], "").await;
    let (manager, workspace) = (&fixture.manager, &fixture.workspaces[0]);
    fixture.link_as(workspace, Assigner::Agent).await;
    manager.poll_linked_item_activity().await.unwrap();
    fixture.assign_elsewhere(&[]);
    manager.poll_linked_item_activity().await.unwrap();
    manager.settle_requested_agents().await;
    assert_eq!(
        fixture.changes(),
        [json!(["shoal-agent"]), json!(["shoal-agent"])]
    );
}

#[tokio::test]
async fn both_assigns_the_user_and_the_agent_and_takes_both_back() {
    let fixture = agents(&["fix"], &[], "[issue]\nassign = \"both\"\n").await;
    let workspace = &fixture.workspaces[0];
    assert!(fixture.link_as(workspace, Assigner::Agent).await.is_empty());
    fixture.unlink(workspace).await;
    assert_eq!(
        fixture.changes(),
        [
            json!(["shoal-user"]),
            json!(["shoal-user", "shoal-agent"]),
            json!(["shoal-agent"]),
            json!([]),
        ]
    );
}

#[tokio::test]
async fn under_user_the_agent_account_is_never_assigned() {
    let fixture = agents(&["fix"], &[], "[issue]\nassign = \"user\"\n").await;
    fixture
        .link_as(&fixture.workspaces[0], Assigner::Agent)
        .await;
    assert_eq!(fixture.change_tokens(), ["token user"]);
}

#[tokio::test]
async fn a_moved_space_leaves_the_agent_account_to_the_other_machine() {
    let fixture = agents(&["fix"], &[], "").await;
    let workspace = &fixture.workspaces[0];
    fixture.link_as(workspace, Assigner::Agent).await;
    let manager = &fixture.manager;
    manager.hand_over_assignees(&workspace.id).await.unwrap();
    remove(&fixture, workspace).await;
    assert_eq!(fixture.changes(), [json!(["shoal-agent"])]);
}

#[tokio::test]
async fn a_relinked_agents_assignment_puts_the_agent_account_on_again() {
    let fixture = agents(&["fix"], &[], "").await;
    let workspace = &fixture.workspaces[0];
    let url = format!("http://{}/team/repo/issues/7", fixture.server.host);
    let url: ItemRef = url.parse().unwrap();
    let links = crate::model::SpaceLinks {
        issues: vec![crate::model::LinkedItem {
            url: url.clone(),
            title: None,
        }],
        assignees: vec![crate::model::IssueAssignee {
            url,
            account: Assigner::Agent,
            added: false,
        }],
        ..Default::default()
    };
    let warnings = (fixture.manager.relink_space(&workspace.id, &links, None)).await;
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(fixture.change_tokens(), ["token agent"]);
}

/// Save the stop of the agent `execution` ran in `workspace`, with a
/// restore request when Shoal stopped it to restore it on its own.
fn save_stop(manager: &Manager, workspace: &Workspace, execution: &ExecutionId, asleep: bool) {
    let record = recovery::Record::new(&TrackedAgent::named("claude"), None, Stage::Stopped, None);
    let restore = asleep.then(Default::default);
    recovery::save_stopped(&manager.paths, &workspace.id, execution, record, restore).unwrap();
}

async fn finish(manager: &Manager, execution: ExecutionId) {
    (manager.finish_execution(execution, ExecutionKind::Command, Some(0)))
        .await
        .unwrap();
    manager.settle_requested_agents().await;
}

#[tokio::test]
async fn an_agent_starting_puts_the_agent_account_on_and_exiting_takes_it_off() {
    let fixture = agents(&["fix"], &[], "").await;
    let (manager, workspace) = (&fixture.manager, &fixture.workspaces[0]);
    fixture.link(workspace).await;
    let execution = run_agent(manager, workspace).await;
    manager.settle_requested_agents().await;
    assert_eq!(fixture.changes(), [json!(["shoal-agent"])]);
    finish(manager, execution).await;
    assert_eq!(fixture.changes(), [json!(["shoal-agent"]), json!([])]);
}

#[tokio::test]
async fn an_asleep_agent_keeps_the_agent_account() {
    let fixture = agents(&["fix"], &[], "").await;
    let (manager, workspace) = (&fixture.manager, &fixture.workspaces[0]);
    fixture.link(workspace).await;
    let execution = run_agent(manager, workspace).await;
    save_stop(manager, workspace, &execution, true);
    finish(manager, execution).await;
    assert_eq!(fixture.changes(), [json!(["shoal-agent"])]);
}

#[tokio::test]
async fn a_stopped_agent_gives_up_the_issue_an_agent_assigned_until_restored() {
    let fixture = agents(&["fix"], &[], "").await;
    let (manager, workspace) = (&fixture.manager, &fixture.workspaces[0]);
    fixture.link_as(workspace, Assigner::Agent).await;
    let execution = run_agent(manager, workspace).await;
    save_stop(manager, workspace, &execution, false);
    finish(manager, execution.clone()).await;
    assert_eq!(fixture.changes(), [json!(["shoal-agent"]), json!([])]);
    let selector = SpaceSelector::id(&workspace.id);
    (manager.queue_stopped_restore(&selector, &execution, Default::default()))
        .await
        .unwrap();
    manager.settle_requested_agents().await;
    assert_eq!(fixture.changes()[2], json!(["shoal-agent"]));
}

#[tokio::test]
async fn done_takes_the_agent_account_off_and_undoing_it_puts_it_back() {
    let fixture = agents(&["fix"], &[], "").await;
    let (manager, workspace) = (&fixture.manager, &fixture.workspaces[0]);
    fixture.link_as(workspace, Assigner::Agent).await;
    let selector = SpaceSelector::id(&workspace.id);
    let done = Done::default();
    (manager.mark_space_done(&selector, done, &Attacher::User))
        .await
        .unwrap();
    manager.settle_requested_agents().await;
    manager.undo_space_done(&selector).await.unwrap();
    manager.settle_requested_agents().await;
    assert_eq!(
        fixture.changes(),
        [json!(["shoal-agent"]), json!([]), json!(["shoal-agent"])]
    );
}

#[tokio::test]
async fn a_chats_agent_gives_up_its_issue_once_done() {
    let fixture = agents(&[], &[], "").await;
    let manager = &fixture.manager;
    let repository = Some(&fixture.repository);
    let chat = crate::daemon::test_support::chat(manager, "plan", "plan", repository, None).await;
    fixture.link_space(&chat.id, Assigner::User).await;
    let done = Done::default();
    (manager.mark_space_done(&SpaceSelector::id(&chat.id), done, &Attacher::User))
        .await
        .unwrap();
    manager.settle_requested_agents().await;
    assert_eq!(fixture.changes(), [json!(["shoal-agent"]), json!([])]);
}

#[tokio::test]
async fn a_queued_launch_puts_the_agent_account_on_until_cancelled() {
    let fixture = agents(&["fix", "first"], &[], "").await;
    let (manager, workspace) = (&fixture.manager, &fixture.workspaces[0]);
    fixture.link(workspace).await;
    // The launch waits for another workspace to finish.
    let blocker = crate::model::Blocker::Space(fixture.workspaces[1].id.to_string());
    let request = crate::model::QueueRequest {
        blockers: vec![blocker],
        ..crate::daemon::workspace::agent_queue::tests::request(&fixture.repository, "fix", false)
    };
    let admission = manager.queue_add(request).await.unwrap();
    assert!(!admission.start);
    let entry = admission.entry.unwrap();
    let selector = SpaceSelector::id(&workspace.id);
    manager.queue_attach(&entry.id, &selector).await.unwrap();
    manager.settle_requested_agents().await;
    assert_eq!(fixture.changes(), [json!(["shoal-agent"])]);
    (manager.queue_remove(entry.id.as_str(), None))
        .await
        .unwrap();
    manager.settle_requested_agents().await;
    assert_eq!(fixture.changes(), [json!(["shoal-agent"]), json!([])]);
}

#[tokio::test]
async fn forgetting_a_stopped_agent_leaves_the_issue_it_gave_up() {
    let fixture = agents(&["fix", "other"], &[], "").await;
    let (manager, workspace) = (&fixture.manager, &fixture.workspaces[0]);
    fixture.link_as(workspace, Assigner::Agent).await;
    let execution = run_agent(manager, workspace).await;
    save_stop(manager, workspace, &execution, false);
    finish(manager, execution).await;
    std::fs::remove_dir_all(manager.paths.space_state(&workspace.id)).unwrap();
    // Another space's link settles the issue again.
    fixture.link(&fixture.workspaces[1]).await;
    assert_eq!(fixture.changes(), [json!(["shoal-agent"]), json!([])]);
}

#[tokio::test]
async fn a_forgotten_agent_execution_gives_up_the_issue() {
    let fixture = agents(&["fix"], &[], "").await;
    let (manager, workspace) = (&fixture.manager, &fixture.workspaces[0]);
    fixture.link_as(workspace, Assigner::Agent).await;
    let execution = run_agent(manager, workspace).await;
    // As after a reboot, whose processes ran in an earlier boot.
    manager.forget_execution(&execution).await.unwrap();
    manager.settle_requested_agents().await;
    assert_eq!(fixture.changes(), [json!(["shoal-agent"]), json!([])]);
}

#[tokio::test]
async fn stopping_an_asleep_agent_takes_the_agent_account_off() {
    let fixture = agents(&["fix"], &[], "").await;
    let (manager, workspace) = (&fixture.manager, &fixture.workspaces[0]);
    fixture.link(workspace).await;
    let execution = run_agent(manager, workspace).await;
    save_stop(manager, workspace, &execution, true);
    finish(manager, execution).await;
    let stop = crate::daemon::workspace::StopRequest::manual();
    (manager.stop_space(&SpaceSelector::id(&workspace.id), stop))
        .await
        .unwrap();
    manager.settle_requested_agents().await;
    assert_eq!(fixture.changes(), [json!(["shoal-agent"]), json!([])]);
}
