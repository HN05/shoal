//! A scripted forge whose issue the tests of issue links assign.
use shoal_protocol::executions::TrackedAgent;
use std::sync::{Arc, Mutex};

use crate::daemon::test_support::{configure, git, manager, repository};
use crate::{
    daemon::{executions::ExecutionKind, manager::Manager},
    model::{Assigner, Workspace},
};
use serde_json::{Value, json};
use shoal_forge::test_support::Server;
use shoal_ids::{execution_ids::ExecutionId, space_ids::SpaceId};
use shoal_protocol::space_names::SpaceSelector;

/// Which assignee edits the scripted forge refuses.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Refuse {
    Nothing,
    Adding,
    Removing,
}

/// A Forgejo repository `team/repo` whose open issue 7 starts with the
/// `assignees` and changes them as edits say, unless it refuses them, where
/// the user's token signs in as `shoal-user` and the agent's as
/// `shoal-agent`, and a manager under `config` with a workspace of each of
/// `names` in a repository that has it as its origin.
pub(crate) struct Fixture {
    _root: tempfile::TempDir,
    pub(crate) manager: std::sync::Arc<Manager>,
    pub(crate) server: Server,
    pub(crate) repository: crate::model::Repository,
    pub(crate) workspaces: Vec<Workspace>,
    assignees: Arc<Mutex<Vec<Value>>>,
    refuse: Arc<Mutex<Refuse>>,
}

/// The config that assigns the user's own login.
pub(crate) const USER: &str = "[issue]\nassign = \"user\"\n";

/// The forge host's setting that gives the agent account a token.
pub(crate) const AGENT_TOKEN: &str = "agent_token = \"agent\"\n";

impl Fixture {
    pub(crate) async fn new(names: &[&str], assignees: &[&str], config: &str) -> Self {
        Self::start(names, assignees, config, "").await
    }

    /// [`Self::new`] with the forge host's `settings` beside the user's
    /// token.
    pub(crate) async fn start(
        names: &[&str],
        assignees: &[&str],
        config: &str,
        settings: &str,
    ) -> Self {
        let (root, manager) = manager().await;
        let path = repository(root.path(), "repo");
        let repository = manager
            .repositories
            .register(path.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let mut workspaces = Vec::new();
        for name in names {
            let created =
                manager.create_workspace(&repository.id, (*name).into(), None, None, None);
            workspaces.push(created.await.unwrap());
        }
        let assignees = Arc::new(Mutex::new(logins(assignees)));
        let refuse = Arc::new(Mutex::new(Refuse::Nothing));
        let refusing = refuse.clone();
        let assigned = assignees.clone();
        let server = Server::start(move |request| {
            let body = match request.line.as_str() {
                "GET /api/v1/user" => match request.header("Authorization") {
                    Some("token agent") => json!({"login": "shoal-agent"}),
                    _ => json!({"login": "shoal-user"}),
                },
                "GET /api/v1/repos/team/repo/issues/7" => json!({"number": 7,
                    "title": "Bug", "state": "open", "assignees": *assigned.lock().unwrap()}),
                "PATCH /api/v1/repos/team/repo/issues/7" => {
                    let edit: Value = serde_json::from_str(&request.body).unwrap();
                    let edited: Vec<&str> = (edit["assignees"].as_array().unwrap().iter())
                        .map(|login| login.as_str().unwrap())
                        .collect();
                    let mut current = assigned.lock().unwrap();
                    let change = match edited.len() > current.len() {
                        true => Refuse::Adding,
                        false => Refuse::Removing,
                    };
                    if *refusing.lock().unwrap() == change {
                        return (403, Vec::new(), r#"{"message":"forbidden"}"#.into());
                    }
                    *current = logins(&edited);
                    json!({})
                }
                _ => return (404, Vec::new(), Vec::new()),
            };
            (200, Vec::new(), body.to_string().into())
        });
        let host = &server.host;
        crate::forge::hosts::learn(host, crate::forge::backends::ForgeKind::Forgejo);
        let forge = format!(
            "[machine.forges.\"{host}\"]\ntype = \"forgejo\"\nuser_token = \"user\"\n{settings}"
        );
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
            repository,
            workspaces,
            assignees,
            refuse,
        }
    }

    /// Change issue 7's assignees to `logins` behind Shoal's back, as
    /// someone else editing it on the forge does.
    pub(crate) fn assign_elsewhere(&self, logins: &[&str]) {
        *self.assignees.lock().unwrap() = self::logins(logins);
    }

    /// Refuse the assignee edits of `change` from now on.
    pub(crate) fn refuse(&self, change: Refuse) {
        *self.refuse.lock().unwrap() = change;
    }

    /// Link issue 7 to `workspace` as assigned, as the user's terminal does.
    pub(crate) async fn link(&self, workspace: &Workspace) -> Vec<String> {
        self.link_as(workspace, Assigner::User).await
    }

    /// Link issue 7 to `workspace` as assigned by `assigner`.
    pub(crate) async fn link_as(&self, workspace: &Workspace, assigner: Assigner) -> Vec<String> {
        self.link_space(&workspace.id, assigner).await
    }

    /// Link issue 7 to the space `id` as assigned by `assigner`.
    pub(crate) async fn link_space(&self, id: &SpaceId, assigner: Assigner) -> Vec<String> {
        let selector = SpaceSelector::id(id);
        (self.manager.links.set_issue(
            &*self.manager,
            &selector,
            &"7".parse().unwrap(),
            None,
            assigner,
        ))
        .await
        .unwrap()
    }

    pub(crate) async fn unlink(&self, workspace: &Workspace) -> Vec<String> {
        let selector = SpaceSelector::id(&workspace.id);
        self.manager
            .links
            .clear_issue(&*self.manager, &selector, None)
            .await
            .unwrap()
    }

    /// The token of each assignee edit the forge received, in order.
    pub(crate) fn change_tokens(&self) -> Vec<String> {
        let received = self.server.received();
        (received.iter())
            .filter(|request| request.line.starts_with("PATCH"))
            .map(|request| request.header("Authorization").unwrap().to_owned())
            .collect()
    }

    /// Each set of assignees the forge was asked to change to, in order.
    pub(crate) fn changes(&self) -> Vec<Value> {
        let received = self.server.received();
        (received.iter())
            .filter(|request| request.line.starts_with("PATCH"))
            .map(|request| {
                serde_json::from_str::<Value>(&request.body).unwrap()["assignees"].clone()
            })
            .collect()
    }
}

pub(crate) fn logins(names: &[&str]) -> Vec<Value> {
    (names.iter())
        .map(|login| json!({"login": login}))
        .collect()
}

/// Start a tracked agent in `workspace`, which runs until its execution
/// finishes.
pub(crate) async fn run_agent(manager: &Manager, workspace: &Workspace) -> ExecutionId {
    let execution = manager
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, None)
        .await
        .unwrap();
    let agent = TrackedAgent::named("claude");
    let id = execution.plan.id.clone();
    (manager.track_agent(
        &id,
        &agent,
        &workspace.id,
        &workspace.name,
        false,
        Default::default(),
    ))
    .await;
    id
}
