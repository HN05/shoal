use super::*;
use crate::daemon::links::issue::test_support::{AGENT_TOKEN, Fixture};
use crate::removal::{InspectionPolicy, UnsavedWork};
use serde_json::json;

/// A fixture whose agent account has a token, under `config`.
async fn agents(names: &[&str], assignees: &[&str], config: &str) -> Fixture {
    Fixture::start(names, assignees, config, AGENT_TOKEN).await
}

async fn remove(fixture: &Fixture, workspace: &crate::model::Workspace) {
    let selector = SpaceSelector::id(&workspace.id);
    (fixture.manager)
        .remove_workspace(&selector, UnsavedWork::default(), InspectionPolicy::GitOnly)
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
