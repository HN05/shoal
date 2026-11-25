use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use shoal_core::test_support::{git, global_config, repository};
use shoal_forge::test_support::Server;
use shoal_forge_types::backends::ForgeKind;
use shoal_protocol::{
    model::{Assigner, Workspace},
    space_names::SpaceSelector,
};

use crate::{
    Links,
    stand_in::{StandIn, record, service},
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
    assignees: Arc<Mutex<Vec<Value>>>,
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
        let assignees = Arc::new(Mutex::new(logins(assignees)));
        let server = issue_forge(assignees.clone(), refuse.clone());
        let host = &server.host;
        shoal_forge_types::hosts::learn(host, ForgeKind::Forgejo);
        let text = format!(
            "{config}\n[machine.forges.\"{host}\"]\ntype = \"forgejo\"\nuser_token = \"user\"\n{settings}"
        );
        let origin = format!("http://{host}/team/repo");
        git(&path, &["remote", "add", "origin", &origin]);
        let links = service(root.path(), store, &text);
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
            assignees,
            refuse,
        }
    }

    /// Make `logins` the issue's assignees, as another Shoal or person does.
    fn assign_elsewhere(&self, logins: &[&str]) {
        *self.assignees.lock().unwrap() = self::logins(logins);
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

/// The scripted forge whose issue 7 has `assigned`, refusing the edits
/// `refuse` names.
fn issue_forge(assigned: Arc<Mutex<Vec<Value>>>, refuse: Arc<Mutex<Refuse>>) -> Server {
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

#[tokio::test]
async fn a_scoped_relink_takes_on_no_assignment() {
    use shoal_forge_types::items::reference::ItemRef;
    use shoal_protocol::model::{IssueAssignee, LinkedItem, SpaceLinks};
    let fixture = Fixture::new(&["shoal-user"], USER).await;
    // The links of a space whose issue 7 has the user's login, which Shoal
    // added.
    let url: ItemRef = format!("http://{}/team/repo/issues/7", fixture.server.host)
        .parse()
        .unwrap();
    let carried = SpaceLinks {
        issues: vec![LinkedItem {
            url: url.clone(),
            title: None,
        }],
        assignees: vec![IssueAssignee {
            url,
            account: Assigner::User,
            added: true,
        }],
        ..Default::default()
    };
    let caller = shoal_daemon_base::callers::Caller::of_space(fixture.workspace.id.clone());
    let warnings = (fixture.links).relink_space(
        &fixture.daemon,
        &fixture.workspace.id,
        &carried,
        Some(&caller),
    );
    let warnings = warnings.await;
    assert!(warnings.is_empty(), "{warnings:?}");
    // The login was there already and the claim was ignored, so nothing
    // takes it back.
    fixture.unlink().await;
    assert!(fixture.changes().is_empty());
}

/// A fixture whose agent account has a token, assigning it by default.
async fn agents() -> Fixture {
    Fixture::start(&[], "", AGENT_TOKEN).await
}

#[tokio::test]
async fn the_agent_account_another_shoal_took_off_goes_back_on() {
    let fixture = agents().await;
    fixture.link_as(Assigner::Agent).await;
    let poll = || fixture.links.poll_linked_item_activity(&fixture.daemon);
    poll().await.unwrap();
    fixture.assign_elsewhere(&[]);
    poll().await.unwrap();
    fixture.links.settle_requested_agents(&fixture.daemon).await;
    assert_eq!(
        fixture.changes(),
        [json!(["shoal-agent"]), json!(["shoal-agent"])]
    );
}

#[tokio::test]
async fn a_relinked_agents_assignment_puts_the_agent_account_on_again() {
    use shoal_forge_types::items::reference::ItemRef;
    use shoal_protocol::model::{IssueAssignee, LinkedItem, SpaceLinks};
    let fixture = agents().await;
    let url: ItemRef = format!("http://{}/team/repo/issues/7", fixture.server.host)
        .parse()
        .unwrap();
    let carried = SpaceLinks {
        issues: vec![LinkedItem {
            url: url.clone(),
            title: None,
        }],
        assignees: vec![IssueAssignee {
            url,
            account: Assigner::Agent,
            added: false,
        }],
        ..Default::default()
    };
    let id = &fixture.workspace.id;
    let warnings = fixture
        .links
        .relink_space(&fixture.daemon, id, &carried, None);
    let warnings = warnings.await;
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(fixture.change_tokens(), ["token agent"]);
}

#[tokio::test]
async fn an_agent_starting_puts_the_agent_account_on_and_exiting_takes_it_off() {
    let fixture = agents().await;
    fixture.link().await;
    let (agents, id) = (&fixture.daemon.agents, &fixture.workspace.id);
    let settle = || fixture.links.settle_agent_assignees(&fixture.daemon, id);
    agents.lock().unwrap().insert(id.clone());
    settle().await;
    assert_eq!(fixture.changes(), [json!(["shoal-agent"])]);
    agents.lock().unwrap().remove(id);
    settle().await;
    assert_eq!(fixture.changes(), [json!(["shoal-agent"]), json!([])]);
}

/// Record a watch's read of issue 7 with `assignees`, `seconds` from now,
/// returning the updates it reports.
async fn observe_issue(
    fixture: &Fixture,
    assignees: &[&str],
    seconds: u64,
) -> Vec<shoal_forge_types::updates::Update> {
    use shoal_forge_types::items::link::Selection;
    let url = format!("http://{}/team/repo/issues/7", fixture.server.host);
    let snapshot = serde_json::from_value(json!({"comments": {}, "checks": {},
        "conflict": null, "state": "open", "errors": {}, "details": {"title": "Bug",
        "body": "", "labels": [], "assignees": assignees}}))
    .unwrap();
    let at = shoal_protocol::time::UnixTime::now() + std::time::Duration::from_secs(seconds);
    let (all, report) = (
        Selection::default(),
        crate::wait::branch_report::BranchReport::Conflicts,
    );
    let start = fixture.links.poll_start_at(at);
    let id = &fixture.workspace.id;
    let observations = vec![(url, snapshot)];
    let recorded =
        (fixture.links).record_observed(&fixture.daemon, id, observations, &all, report, start);
    recorded.await.unwrap()
}

#[tokio::test]
async fn the_agent_account_an_agents_start_puts_on_is_the_spaces_own_change() {
    use shoal_forge_types::updates::UpdateKind;
    let fixture = agents().await;
    fixture.link().await;
    assert!(observe_issue(&fixture, &[], 1).await.is_empty());
    let id = &fixture.workspace.id;
    fixture.daemon.agents.lock().unwrap().insert(id.clone());
    fixture
        .links
        .settle_agent_assignees(&fixture.daemon, id)
        .await;
    assert_eq!(fixture.changes(), [json!(["shoal-agent"])]);
    let own = observe_issue(&fixture, &["shoal-agent"], 2).await;
    assert!(own.is_empty(), "{own:?}");
    let updates = observe_issue(&fixture, &["shoal-agent", "ann"], 3).await;
    let [edited] = &updates[..] else {
        panic!("{updates:?}");
    };
    assert_eq!(edited.kind, UpdateKind::Edited);
    assert!(edited.message.contains("ann"), "{}", edited.message);
}
