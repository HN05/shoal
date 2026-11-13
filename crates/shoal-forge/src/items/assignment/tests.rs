use std::sync::{Arc, Mutex};

use super::*;
use crate::{
    backends::ForgeKind,
    test_support::{Server, agent_on, repo_on},
    transport::memory::Memory,
};
use serde_json::{Value, json};

/// A GitHub issue 7 in `state` whose assignees start as `assignees`, and
/// change as edits replace them.
fn issue_server(state: &'static str, assignees: &[&str]) -> Server {
    let assignees = Arc::new(Mutex::new(
        (assignees.iter())
            .map(|login| json!({"login": login}))
            .collect::<Vec<_>>(),
    ));
    Server::start(move |request| {
        let body = match request.line.as_str() {
            "GET /api/v3/user" => json!({"login": "Agent"}),
            "PATCH /api/v3/repos/team/repo/issues/7" => {
                let edit: Value = serde_json::from_str(&request.body).unwrap();
                let logins = edit["assignees"].as_array().unwrap();
                *assignees.lock().unwrap() = (logins.iter())
                    .map(|login| json!({"login": login}))
                    .collect();
                json!({})
            }
            _ => json!({"number": 7, "title": "Bug", "state": state,
                "assignees": *assignees.lock().unwrap()}),
        };
        (200, Vec::new(), body.to_string().into())
    })
}

fn changes(server: &Server) -> Vec<String> {
    (server.received().into_iter())
        .filter(|request| !request.line.starts_with("GET"))
        .map(|request| format!("{} {}", request.line, request.body))
        .collect()
}

fn issue(server: &Server) -> (ForgeRepo, ItemRef) {
    let repo = repo_on(server, ForgeKind::GitHub, "team/repo");
    let url = format!("http://{}/team/repo/issues/7", server.host);
    (repo, url.parse().unwrap())
}

#[tokio::test]
async fn assigning_adds_the_accounts_login_and_names_the_other_assignees() {
    let server = issue_server("open", &["ann"]);
    let account = agent_on(&server, "agent").remembering(Memory::default());
    let (repo, issue) = issue(&server);
    let assignment = repo.assign_own(&account, &issue).await.unwrap();
    assert_eq!(
        assignment.unwrap(),
        Assignment {
            login: "Agent".into(),
            added: true,
            others: vec!["ann".into()],
        }
    );
    let patch = r#"PATCH /api/v3/repos/team/repo/issues/7 {"assignees":["ann","Agent"]}"#;
    assert_eq!(changes(&server), [patch]);

    // Removing it leaves the other assignee.
    assert!(repo.unassign(&account, &issue, "agent").await.unwrap());
    let patch = r#"PATCH /api/v3/repos/team/repo/issues/7 {"assignees":["ann"]}"#;
    assert_eq!(changes(&server)[1..], [patch]);
}

#[tokio::test]
async fn an_issue_the_account_has_already_is_left_as_it_is() {
    let server = issue_server("open", &["agent"]);
    let account = agent_on(&server, "agent").remembering(Memory::default());
    let (repo, issue) = issue(&server);
    let assignment = repo.assign_own(&account, &issue).await.unwrap().unwrap();
    assert!(!assignment.added);
    assert!(assignment.others.is_empty());
    assert!(changes(&server).is_empty());
}

#[tokio::test]
async fn a_closed_issue_is_neither_assigned_nor_unassigned() {
    let server = issue_server("closed", &["Agent"]);
    let account = agent_on(&server, "agent").remembering(Memory::default());
    let (repo, issue) = issue(&server);
    assert!(!repo.unassign(&account, &issue, "Agent").await.unwrap());
    let unassigned = issue_server("closed", &[]);
    let (repo, issue) = self::issue(&unassigned);
    let account = agent_on(&unassigned, "agent").remembering(Memory::default());
    assert!(
        !repo
            .assign_own(&account, &issue)
            .await
            .unwrap()
            .unwrap()
            .added
    );
    assert!(changes(&server).is_empty() && changes(&unassigned).is_empty());
}

#[tokio::test]
async fn an_account_without_its_own_login_assigns_nothing() {
    let server = issue_server("open", &[]);
    let account = Account::agent(Default::default(), Default::default());
    let (repo, issue) = issue(&server);
    assert_eq!(repo.assign_own(&account, &issue).await.unwrap(), None);
    assert!(server.received().is_empty());
}
