use std::sync::{Arc, Mutex};

use super::*;
use crate::client::space_names::SpaceSelector;
use crate::daemon::test_support::{configure, git, manager, repository};
use crate::forge::test_support::Server;
use crate::{
    model::Workspace,
    removal::{InspectionPolicy, UnsavedWork},
};
use serde_json::{Value, json};

/// Which assignee edits the scripted forge refuses.
#[derive(Clone, Copy, PartialEq)]
enum Refuse {
    Nothing,
    Adding,
    Removing,
}

/// A Forgejo repository `team/repo` whose open issue 7 starts with the
/// `assignees` and changes them as edits say, unless it refuses them, where
/// every token signs in as `shoal-user`, and a manager under `config` with a
/// workspace of each of `names` in a repository that has it as its origin.
struct Fixture {
    _root: tempfile::TempDir,
    manager: std::sync::Arc<Manager>,
    server: Server,
    workspaces: Vec<Workspace>,
    refuse: Arc<Mutex<Refuse>>,
}

impl Fixture {
    async fn new(names: &[&str], assignees: &[&str], config: &str) -> Self {
        Self::start(names, assignees, config, "").await
    }

    /// [`Self::new`] with the forge host's `settings` beside the user's
    /// token.
    async fn start(names: &[&str], assignees: &[&str], config: &str, settings: &str) -> Self {
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
        let refuse = Arc::new(Mutex::new(Refuse::Nothing));
        let refusing = refuse.clone();
        let server = Server::start(move |request| {
            let body = match request.line.as_str() {
                "GET /api/v1/user" => json!({"login": "shoal-user"}),
                "GET /api/v1/repos/team/repo/issues/7" => json!({"number": 7,
                    "title": "Bug", "state": "open", "assignees": *assignees.lock().unwrap()}),
                "PATCH /api/v1/repos/team/repo/issues/7" => {
                    let edit: Value = serde_json::from_str(&request.body).unwrap();
                    let edited: Vec<&str> = (edit["assignees"].as_array().unwrap().iter())
                        .map(|login| login.as_str().unwrap())
                        .collect();
                    let mut current = assignees.lock().unwrap();
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
            workspaces,
            refuse,
        }
    }

    /// Refuse the assignee edits of `change` from now on.
    fn refuse(&self, change: Refuse) {
        *self.refuse.lock().unwrap() = change;
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

    async fn unlink(&self, workspace: &Workspace) -> Vec<String> {
        let selector = SpaceSelector::id(&workspace.id);
        self.manager.clear_issue(&selector, None).await.unwrap()
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
async fn unlinking_takes_back_the_assignee_shoal_added() {
    let fixture = Fixture::new(&["fix"], &["ann"], "").await;
    let workspace = &fixture.workspaces[0];
    fixture.link(workspace).await;
    assert!(fixture.unlink(workspace).await.is_empty());
    assert_eq!(
        fixture.changes(),
        [json!(["ann", "shoal-user"]), json!(["ann"])]
    );
}

#[tokio::test]
async fn an_assignee_shoal_did_not_add_stays() {
    let fixture = Fixture::new(&["fix"], &["shoal-user"], "").await;
    let workspace = &fixture.workspaces[0];
    fixture.link(workspace).await;
    fixture.unlink(workspace).await;
    assert!(fixture.changes().is_empty());
}

#[tokio::test]
async fn the_assignee_stays_while_another_space_of_the_login_links_the_issue() {
    let fixture = Fixture::new(&["first", "second"], &[], "").await;
    let [first, second] = &fixture.workspaces[..] else {
        unreachable!()
    };
    fixture.link(first).await;
    fixture.link(second).await;
    fixture.unlink(first).await;
    assert_eq!(fixture.changes(), [json!(["shoal-user"])]);
    // Removing the last space that links it takes it back.
    let selector = SpaceSelector::id(&second.id);
    (fixture.manager)
        .remove_workspace(&selector, UnsavedWork::default(), InspectionPolicy::GitOnly)
        .await
        .unwrap();
    assert_eq!(fixture.changes(), [json!(["shoal-user"]), json!([])]);
}

#[tokio::test]
async fn an_assignee_a_removal_cannot_take_back_notifies_the_user() {
    let fixture = Fixture::new(&["fix"], &[], "").await;
    fixture.refuse(Refuse::Removing);
    let workspace = &fixture.workspaces[0];
    fixture.link(workspace).await;
    let selector = SpaceSelector::id(&workspace.id);
    (fixture.manager)
        .remove_workspace(&selector, UnsavedWork::default(), InspectionPolicy::GitOnly)
        .await
        .unwrap();
    let notifications = fixture.manager.notifications(true, 10).await.unwrap();
    let kept = (notifications.iter())
        .find(|notification| notification.kind == NotificationKind::UnassignFailed)
        .unwrap();
    assert!(
        kept.message.contains("--remove-assignee shoal-user"),
        "{}",
        kept.message
    );
}

#[tokio::test]
async fn a_moved_space_leaves_its_assignee_to_the_other_machine() {
    let fixture = Fixture::new(&["fix"], &[], "").await;
    let workspace = &fixture.workspaces[0];
    fixture.link(workspace).await;
    let manager = &fixture.manager;
    manager.hand_over_assignees(&workspace.id).await.unwrap();
    let selector = SpaceSelector::id(&workspace.id);
    (manager.remove_workspace(&selector, UnsavedWork::default(), InspectionPolicy::GitOnly))
        .await
        .unwrap();
    assert_eq!(fixture.changes(), [json!(["shoal-user"])]);
}

#[tokio::test]
async fn a_restored_space_assigns_its_issue_as_the_account_that_had_it() {
    let agent = "agent_token = \"agent\"\n";
    let fixture = Fixture::start(&["fix"], &[], "", agent).await;
    let workspace = &fixture.workspaces[0];
    let (manager, selector) = (&fixture.manager, SpaceSelector::id(&workspace.id));
    (manager.set_issue(&selector, &"7".parse().unwrap(), None, Assigner::Agent))
        .await
        .unwrap();
    (manager.remove_workspace(&selector, UnsavedWork::default(), InspectionPolicy::GitOnly))
        .await
        .unwrap();
    let restored = manager.restore_workspace(&selector).await.unwrap();
    assert!(restored.warnings.is_empty(), "{:?}", restored.warnings);
    assert_eq!(fixture.change_tokens(), ["token agent"; 3]);
}

#[tokio::test]
async fn a_restored_space_takes_back_the_assignee_shoal_added_before() {
    let fixture = Fixture::new(&["fix"], &[], "").await;
    let workspace = &fixture.workspaces[0];
    let (manager, selector) = (&fixture.manager, SpaceSelector::id(&workspace.id));
    fixture.link(workspace).await;
    // The removal could not take it back, so the forge still shows it.
    fixture.refuse(Refuse::Removing);
    (manager.remove_workspace(&selector, UnsavedWork::default(), InspectionPolicy::GitOnly))
        .await
        .unwrap();
    fixture.refuse(Refuse::Nothing);
    let restored = manager.restore_workspace(&selector).await.unwrap();
    fixture.unlink(&restored.opened.workspace).await;
    assert_eq!(
        fixture.changes(),
        [json!(["shoal-user"]), json!([]), json!([])]
    );
}

/// The links of a space whose issue 7 the agent account had, which Shoal
/// added.
fn carried(fixture: &Fixture) -> crate::model::SpaceLinks {
    let url = format!("http://{}/team/repo/issues/7", fixture.server.host);
    let url: ItemRef = url.parse().unwrap();
    crate::model::SpaceLinks {
        issues: vec![crate::model::LinkedItem {
            url: url.clone(),
            title: None,
        }],
        assignees: vec![IssueAssignee {
            url,
            account: Assigner::Agent,
            added: true,
        }],
        ..Default::default()
    }
}

#[tokio::test]
async fn a_relinked_space_assigns_as_the_account_its_links_carry_for_the_user() {
    let agent = "agent_token = \"agent\"\n";
    let fixture = Fixture::start(&["fix"], &[], "", agent).await;
    let workspace = &fixture.workspaces[0];
    let links = carried(&fixture);
    let warnings = (fixture.manager.relink_space(&workspace.id, &links, None)).await;
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(fixture.change_tokens(), ["token agent"]);
}

#[tokio::test]
async fn a_scoped_relink_assigns_as_the_agent_and_takes_on_nothing() {
    let agent = "agent_token = \"agent\"\n";
    let fixture = Fixture::start(&["fix"], &["shoal-user"], "", agent).await;
    let workspace = &fixture.workspaces[0];
    let mut links = carried(&fixture);
    links.assignees[0].account = Assigner::User;
    let caller = crate::daemon::scope::Caller {
        space_id: workspace.id.clone(),
        execution: None,
    };
    let warnings = (fixture
        .manager
        .relink_space(&workspace.id, &links, Some(&caller)))
    .await;
    assert!(warnings.is_empty(), "{warnings:?}");
    // The login was there already and the claim was ignored, so nothing
    // takes it back.
    fixture.unlink(workspace).await;
    assert!(fixture.changes().is_empty());
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
    let fixture = Fixture::new(&["fix"], &[], "").await;
    fixture.refuse(Refuse::Adding);
    let workspace = &fixture.workspaces[0];
    let warnings = fixture.link(workspace).await;
    assert!(warnings[0].contains("forbidden"), "{warnings:?}");
    // Linking it again tries again.
    assert!(fixture.link(workspace).await[0].contains("forbidden"));
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
    fixture.unlink(workspace).await;
    assert!(fixture.server.received().is_empty());
}
