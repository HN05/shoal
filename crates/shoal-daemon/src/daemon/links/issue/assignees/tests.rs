use std::sync::{Arc, Mutex};

use super::*;
use crate::client::space_names::SpaceSelector;
use crate::daemon::test_support::{configure, git, manager, repository};
use crate::forge::test_support::Server;
use crate::model::Workspace;
use serde_json::{Value, json};

/// A Forgejo repository `team/repo` whose open issue 7 starts with the
/// `assignees` and changes them as edits say, unless it refuses them, where
/// the user's token signs in as `shoal-user`, and a manager under `config`
/// with a workspace of each of `names` in a repository that has it as its
/// origin.
struct Fixture {
    _root: tempfile::TempDir,
    manager: std::sync::Arc<Manager>,
    server: Server,
    workspaces: Vec<Workspace>,
}

impl Fixture {
    async fn new(names: &[&str], assignees: &[&str], config: &str) -> Self {
        Self::start(names, assignees, config, false).await
    }

    async fn start(names: &[&str], assignees: &[&str], config: &str, refuse: bool) -> Self {
        let (root, manager) = manager().await;
        let path = repository(root.path(), "repo");
        let repository = manager
            .register_repository(path.to_str().unwrap().into(), None, None)
            .await
            .unwrap()
            .id;
        let mut workspaces = Vec::new();
        for name in names {
            let created = manager.create_workspace(&repository, (*name).into(), None, None, None);
            workspaces.push(created.await.unwrap());
        }
        let assignees = Arc::new(Mutex::new(logins(assignees)));
        let server = Server::start(move |request| {
            let body = match request.line.as_str() {
                "GET /api/v1/user" => json!({"login": "shoal-user"}),
                "GET /api/v1/repos/team/repo/issues/7" => json!({"number": 7,
                    "title": "Bug", "state": "open", "assignees": *assignees.lock().unwrap()}),
                "PATCH /api/v1/repos/team/repo/issues/7" if refuse => {
                    return (403, Vec::new(), r#"{"message":"forbidden"}"#.into());
                }
                "PATCH /api/v1/repos/team/repo/issues/7" => {
                    let edit: Value = serde_json::from_str(&request.body).unwrap();
                    let edited: Vec<&str> = (edit["assignees"].as_array().unwrap().iter())
                        .map(|login| login.as_str().unwrap())
                        .collect();
                    *assignees.lock().unwrap() = logins(&edited);
                    json!({})
                }
                _ => return (404, Vec::new(), Vec::new()),
            };
            (200, Vec::new(), body.to_string().into())
        });
        let host = &server.host;
        crate::forge::hosts::learn(host, crate::forge::backends::ForgeKind::Forgejo);
        let forge =
            format!("[machine.forges.\"{host}\"]\ntype = \"forgejo\"\nuser_token = \"user\"\n");
        configure(&manager, &format!("{config}\n{forge}")).await;
        git(
            &path,
            &[
                "remote",
                "add",
                "origin",
                &format!("http://{host}/team/repo"),
            ],
        );
        Self {
            _root: root,
            manager,
            server,
            workspaces,
        }
    }

    /// Link issue 7 to `workspace` as assigned, as the user's terminal does.
    async fn link(&self, workspace: &Workspace) -> Vec<String> {
        let selector = SpaceSelector::id(&workspace.id);
        (self
            .manager
            .set_issue(&selector, &"7".parse().unwrap(), None, Assigner::User))
        .await
        .unwrap()
    }

    /// Each set of assignees the forge was asked to change to, in order.
    fn changes(&self) -> Vec<Value> {
        let received = self.server.received();
        (received.iter())
            .filter(|request| request.line.starts_with("PATCH"))
            .map(|request| {
                serde_json::from_str::<Value>(&request.body).unwrap()["assignees"].clone()
            })
            .collect()
    }
}

fn logins(names: &[&str]) -> Vec<Value> {
    (names.iter())
        .map(|login| json!({"login": login}))
        .collect()
}

#[tokio::test]
async fn linking_assigns_the_account_once() {
    let fixture = Fixture::new(&["fix"], &[], "").await;
    let workspace = &fixture.workspaces[0];
    assert!(fixture.link(workspace).await.is_empty());
    assert_eq!(fixture.changes(), [json!(["shoal-user"])]);
    // Linking it again assigns nothing more.
    assert!(fixture.link(workspace).await.is_empty());
    assert_eq!(fixture.changes().len(), 1);
}

#[tokio::test]
async fn linking_an_issue_others_are_assigned_names_them() {
    let fixture = Fixture::new(&["fix"], &["ann", "bob"], "").await;
    let workspace = &fixture.workspaces[0];
    let warnings = fixture.link(workspace).await;
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("ann, bob"), "{warnings:?}");
    assert_eq!(fixture.changes(), [json!(["ann", "bob", "shoal-user"])]);
}

#[tokio::test]
async fn a_refused_assignment_warns_and_keeps_the_link() {
    let fixture = Fixture::start(&["fix"], &[], "", true).await;
    let workspace = &fixture.workspaces[0];
    let warnings = fixture.link(workspace).await;
    assert!(warnings[0].contains("forbidden"), "{warnings:?}");
    let registrations = fixture.manager.issue_registrations(&workspace.id).await;
    assert_eq!(registrations.unwrap().len(), 1);
}

#[tokio::test]
async fn an_account_without_a_token_there_assigns_nothing() {
    let fixture = Fixture::new(&["fix"], &[], "").await;
    let workspace = &fixture.workspaces[0];
    let selector = SpaceSelector::id(&workspace.id);
    // The agent account has no token of its own on the forge.
    let warnings = (fixture.manager)
        .set_issue(&selector, &"7".parse().unwrap(), None, Assigner::Agent)
        .await
        .unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(fixture.server.received().is_empty());
}

#[tokio::test]
async fn assigning_off_leaves_assignees_alone() {
    let fixture = Fixture::new(&["fix"], &[], "[issue]\nassign = false\n").await;
    let workspace = &fixture.workspaces[0];
    assert!(fixture.link(workspace).await.is_empty());
    assert!(fixture.server.received().is_empty());
}
