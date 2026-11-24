use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use shoal_core::test_support::{git, global_config, repository};
use shoal_daemon_base::forge_access::ForgeAccess;
use shoal_daemon_base::operations::Operations;
use shoal_daemon_base::{
    config_state::ConfigState, file_access::FileAccess, gates::Gates, notifications::Notifications,
};
use shoal_forge::test_support::Server;
use shoal_forge_types::backends::ForgeKind;
use shoal_protocol::paths::Paths;
use shoal_protocol::{
    model::{Assigner, Workspace},
    space_names::SpaceSelector,
};

use crate::{
    Links,
    stand_in::{StandIn, StandInRecords},
};

/// The config that assigns the user's own login.
const USER: &str = "[issue]\nassign = \"user\"\n";

/// The forge host's setting that gives the agent account a token.
const AGENT_TOKEN: &str = "agent_token = \"agent\"\n";

/// Which assignee edits the scripted forge refuses.
#[derive(Clone, Copy, PartialEq)]
enum Refuse {
    Nothing,
    Adding,
}

/// A Forgejo repository `team/repo` whose open issue 7 starts with the
/// `assignees` and changes them as edits say, unless it refuses them, where
/// the user's token signs in as `shoal-user` and the agent's as
/// `shoal-agent`, and the links' service over a workspace whose origin it
/// is, under `config`.
struct Fixture {
    _root: tempfile::TempDir,
    links: Arc<Links>,
    daemon: StandIn,
    server: Server,
    workspace: Workspace,
    refuse: Arc<Mutex<Refuse>>,
}

impl Fixture {
    async fn new(assignees: &[&str], config: &str) -> Self {
        Self::start(assignees, config, "").await
    }

    /// [`Self::new`] with the forge host's `settings` beside the user's
    /// token.
    async fn start(assignees: &[&str], config: &str, settings: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let store = shoal_store::test_support::unsynced_store(root.path().join("state.db"))
            .await
            .unwrap();
        let path = repository(root.path(), "repo");
        let workspace = shoal_protocol::test_support::workspace("workspace", "repo", "fix", &path);
        record(&store, &workspace).await;
        let refuse = Arc::new(Mutex::new(Refuse::Nothing));
        let server = issue_forge(assignees, refuse.clone());
        let host = &server.host;
        shoal_forge_types::hosts::learn(host, ForgeKind::Forgejo);
        let text = format!(
            "{config}\n[machine.forges.\"{host}\"]\ntype = \"forgejo\"\nuser_token = \"user\"\n{settings}"
        );
        let origin = format!("http://{host}/team/repo");
        git(&path, &["remote", "add", "origin", &origin]);
        let gates = Arc::new(Gates::default());
        let config = Arc::new(ConfigState::new(global_config(&text)));
        let forges = ForgeAccess::new(store.clone(), gates.clone(), config.clone());
        let links = Arc::new(Links::new(
            store.clone(),
            gates.clone(),
            config,
            Arc::new(Notifications::new(store)),
            Arc::new(FileAccess::default()),
            Arc::new(forges),
            Arc::new(Operations::new(Paths::for_test(root.path()), gates.clone())),
            Arc::new(StandInRecords),
        ));
        let daemon = StandIn {
            workspaces: vec![workspace.clone()],
            config: Some(global_config(&text)),
            ..StandIn::default()
        };
        Self {
            _root: root,
            links,
            daemon,
            server,
            workspace,
            refuse,
        }
    }

    /// Refuse the assignee edits of `change` from now on.
    fn refuse(&self, change: Refuse) {
        *self.refuse.lock().unwrap() = change;
    }

    /// Link issue 7 to the workspace as assigned by `assigner`.
    async fn link_as(&self, assigner: Assigner) -> Vec<String> {
        let selector = SpaceSelector::id(&self.workspace.id);
        let issue = "7".parse().unwrap();
        let linked = self
            .links
            .set_issue(&self.daemon, &selector, &issue, None, assigner);
        linked.await.unwrap()
    }

    /// Link issue 7 to the workspace as the user's terminal does.
    async fn link(&self) -> Vec<String> {
        self.link_as(Assigner::User).await
    }

    async fn unlink(&self) -> Vec<String> {
        let selector = SpaceSelector::id(&self.workspace.id);
        let unlinked = self.links.clear_issue(&self.daemon, &selector, None);
        unlinked.await.unwrap()
    }

    /// The token of each assignee edit the forge received, in order.
    fn change_tokens(&self) -> Vec<String> {
        let received = self.server.received();
        (received.iter())
            .filter(|request| request.line.starts_with("PATCH"))
            .map(|request| request.header("Authorization").unwrap().to_owned())
            .collect()
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

/// The scripted forge whose issue 7 starts with `assignees`, refusing the
/// edits `refuse` names.
fn issue_forge(assignees: &[&str], refuse: Arc<Mutex<Refuse>>) -> Server {
    let assigned = Arc::new(Mutex::new(logins(assignees)));
    Server::start(move |request| {
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
                let adding = edited.len() > current.len();
                if adding && *refuse.lock().unwrap() == Refuse::Adding {
                    return (403, Vec::new(), r#"{"message":"forbidden"}"#.into());
                }
                *current = logins(&edited);
                json!({})
            }
            _ => return (404, Vec::new(), Vec::new()),
        };
        (200, Vec::new(), body.to_string().into())
    })
}

fn logins(names: &[&str]) -> Vec<Value> {
    (names.iter())
        .map(|login| json!({"login": login}))
        .collect()
}

/// Record `workspace` and its repository in `store`.
async fn record(store: &shoal_store::Store, workspace: &Workspace) {
    let path = workspace.path.to_str().unwrap().to_owned();
    store
        .write(move |tx| {
            tx.execute(
                "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo',?1,?1,1)",
                [&path],
            )?;
            tx.execute(
                "INSERT INTO spaces(id,kind,repository_id,name)
                    VALUES ('workspace','workspace','repo','fix')",
                [],
            )?;
            tx.execute(
                "INSERT INTO workspaces(id,path,branch,state,base_commit,git_dir)
                    VALUES ('workspace',?1,'fix','ready','abc','/admin')",
                [&path],
            )?;
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn linking_assigns_the_account_once() {
    let fixture = Fixture::new(&[], USER).await;
    assert!(fixture.link().await.is_empty());
    assert_eq!(fixture.changes(), [json!(["shoal-user"])]);
    // Linking it again assigns nothing more.
    assert!(fixture.link().await.is_empty());
    assert_eq!(fixture.changes().len(), 1);
}

#[tokio::test]
async fn unlinking_takes_back_the_assignee_shoal_added() {
    let fixture = Fixture::new(&["ann"], USER).await;
    fixture.link().await;
    assert!(fixture.unlink().await.is_empty());
    assert_eq!(
        fixture.changes(),
        [json!(["ann", "shoal-user"]), json!(["ann"])]
    );
}

#[tokio::test]
async fn a_refused_assignment_warns_and_keeps_the_link() {
    let fixture = Fixture::new(&[], USER).await;
    fixture.refuse(Refuse::Adding);
    let warnings = fixture.link().await;
    assert!(warnings[0].contains("forbidden"), "{warnings:?}");
    // Linking it again tries again.
    assert!(fixture.link().await[0].contains("forbidden"));
    let registrations = fixture.links.issue_registrations(&fixture.workspace.id);
    assert_eq!(registrations.await.unwrap().len(), 1);
}

#[tokio::test]
async fn an_agents_assignment_adds_the_users_login() {
    let fixture = Fixture::start(&[], USER, AGENT_TOKEN).await;
    let warnings = fixture.link_as(Assigner::Agent).await;
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(fixture.change_tokens(), ["token user"]);
}

#[tokio::test]
async fn assigning_off_leaves_assignees_alone() {
    let fixture = Fixture::new(&[], "[issue]\nassign = \"none\"\n").await;
    assert!(fixture.link().await.is_empty());
    fixture.unlink().await;
    // Only the watch's first read of the issue.
    let lines: Vec<_> = (fixture.server.received().into_iter())
        .map(|request| request.line)
        .collect();
    assert!(
        lines.iter().all(|line| line.starts_with("GET ")),
        "{lines:?}"
    );
}
