//! Starts on a CI run: where the run ran says where the start goes, and
//! the new space links the run.
use std::{path::Path, sync::Arc};

use shoal_core::start_plan::QueuedStart;
use shoal_forge_types::items::reference::ItemInput;
use shoal_protocol::launches::{ItemChoice, Launch, LaunchSource, StartTarget};
use shoal_protocol::protocol::{Launched, operations::StartedWorkspace};
use shoal_protocol::space_names::SpaceSelector;

use crate::daemon::{
    manager::Manager,
    scope::{Access, Caller},
    test_support::{chat, configure, git, manager, repository},
};
use crate::model::{Repository, SpaceKind};

/// A registered clone of a repository with the branch `fix` and the tag
/// `v1.4.0`, whose origin is a Forgejo forge that reports the runs
/// [`run_json`] describes and serves the repository over Git's dumb HTTP
/// protocol.
struct Forge {
    _root: tempfile::TempDir,
    manager: Arc<Manager>,
    repo: Repository,
    url: String,
    _server: shoal_forge::test_support::Server,
}

impl Forge {
    async fn new() -> Self {
        let (root, manager) = manager().await;
        let origin = repository(root.path(), "origin");
        git(&origin, &["branch", "fix"]);
        git(&origin, &["tag", "v1.4.0"]);
        git(&origin, &["update-server-info"]);
        let server = server(&origin);
        let host = server.host.clone();
        crate::forge::hosts::learn(&host, crate::forge::backends::ForgeKind::Forgejo);
        configure(
            &manager,
            &format!(
                "[machine]\nmax_heavy_agents = 1000\n[machine.overload.disk]\nenabled = false\n\
                 [machine.forges.\"{host}\"]\ntype = \"forgejo\"\nuser_token = \"user\"\n\
                 [agents.claude]\ncommand = ['sh']\n"
            ),
        )
        .await;
        let url = format!("http://{host}/team/repo");
        let clone = root.path().join("clone");
        git(
            root.path(),
            &["clone", "-q", origin.to_str().unwrap(), "clone"],
        );
        git(&clone, &["remote", "set-url", "origin", &url]);
        let path = clone.to_str().unwrap().into();
        let repo = (manager.repositories.register(path, None, None))
            .await
            .unwrap();
        Self {
            _root: root,
            manager,
            repo,
            url,
            _server: server,
        }
    }

    /// The URL of the run numbered `number`.
    fn run(&self, number: u64) -> String {
        format!("{}/actions/runs/{number}", self.url)
    }

    /// Start the agent `claude` on the run numbered `number`, as `change`
    /// sets the rest of the launch, for `caller`.
    async fn start(
        &self,
        number: u64,
        change: impl FnOnce(&mut Launch),
        caller: Option<&Caller>,
    ) -> anyhow::Result<Launched> {
        let mut launch = Launch {
            target: Some(self.run(number).parse().unwrap()),
            agent: Some("claude".into()),
            ..Launch::default()
        };
        change(&mut launch);
        let daemon = self.manager.daemon();
        (self.manager.spaces)
            .launch(&daemon, launch, LaunchSource::Manual, caller)
            .await
    }

    /// Create a workspace without an agent on the run numbered `number`.
    async fn create(&self, number: u64) -> anyhow::Result<StartedWorkspace> {
        let launch = Launch {
            target: Some(self.run(number).parse().unwrap()),
            ..Launch::default()
        };
        let daemon = self.manager.daemon();
        (self.manager.spaces)
            .create_placed_workspace(&daemon, launch, (None, None), None)
            .await
    }

    /// The run the agent `launched` queued starts on, as its plan names it.
    async fn started_run(&self, launched: &Launched) -> Option<String> {
        let entry = launched.entry.as_ref().unwrap().id.clone();
        let plan: String = (self.manager.store)
            .run(move |db| shoal_store::test_support::queue::launch_plan(db, entry.as_str()))
            .await
            .unwrap();
        let start = QueuedStart::of(&plan).unwrap();
        start.launch.run.map(|run| run.to_string())
    }

    /// The runs the space `launched` started links as related.
    async fn related(&self, launched: &Launched) -> Vec<String> {
        let space = SpaceSelector::id(&launched.space.id);
        let space = self.manager.spaces.space(&space).await.unwrap();
        (space.links.related.iter())
            .map(|item| item.url.to_string())
            .collect()
    }
}

/// The Forgejo run numbered `number`: run 1 on the default branch, 2 on
/// the tag `v1.4.0`, 3 on the branch `fix` and 4 for PR 9, whose head
/// is `topic`; its one job failed.
fn run_json(number: u64) -> serde_json::Value {
    let (prettyref, event, payload) = match number {
        1 => ("main", "push", String::new()),
        2 => ("v1.4.0", "push", String::new()),
        3 => ("fix", "push", String::new()),
        _ => (
            "topic",
            "pull_request",
            serde_json::json!({"pull_request": {"number": 9}}).to_string(),
        ),
    };
    serde_json::json!({
        "id": 100 + number, "index_in_repo": number, "prettyref": prettyref,
        "workflow_id": "release.yml", "commit_sha": "0123abc", "status": "failure",
        "event": event, "event_payload": payload,
    })
}

/// A Forgejo forge for `team/repo` that reports the runs [`run_json`]
/// describes, their one failed job and PR 9, and serves `origin` over Git's
/// dumb HTTP protocol, from what `git update-server-info` listed.
fn server(origin: &Path) -> shoal_forge::test_support::Server {
    let git_dir = origin.join(".git");
    shoal_forge::test_support::Server::start(move |request| {
        let path = request.line.split(' ').nth(1).unwrap_or_default();
        let (path, query) = path.split_once('?').unwrap_or((path, ""));
        let json = |value: serde_json::Value| (200, Vec::new(), value.to_string().into());
        let api = "/api/v1/repos/team/repo/";
        match path.strip_prefix(api) {
            Some("actions/runs") => {
                let number = query
                    .split('&')
                    .find_map(|pair| pair.strip_prefix("run_number="))
                    .and_then(|number| number.parse().ok())
                    .unwrap_or_default();
                return json(serde_json::json!({"workflow_runs": [run_json(number)]}));
            }
            Some(jobs) if jobs.ends_with("/jobs") => {
                return json(
                    serde_json::json!([{"id": 7, "name": "publish", "status": "failure"}]),
                );
            }
            Some("pulls/9") => {
                return json(serde_json::json!({
                    "number": 9, "title": "Fix the release", "state": "open",
                    "merged": false, "draft": false, "labels": [],
                    "head": {"ref": "topic", "sha": "0123abc", "repo": {"full_name": "team/repo"}},
                    "base": {"ref": "main", "repo": {"full_name": "team/repo"}},
                }));
            }
            _ => {}
        }
        let file = path
            .strip_prefix("/team/repo/")
            .map(|file| git_dir.join(file));
        match file.and_then(|file| std::fs::read(file).ok()) {
            Some(bytes) => (200, Vec::new(), bytes),
            None => (404, Vec::new(), Vec::new()),
        }
    })
}

#[tokio::test]
async fn a_run_on_the_default_branch_starts_a_chat_named_after_it() {
    let forge = Forge::new().await;

    let launched = forge.start(1, |_| {}, None).await.unwrap();

    let space = &launched.space;
    assert_eq!(space.kind(), SpaceKind::Chat);
    assert_eq!(space.name, "release.yml run 1 on main");
    assert_eq!(space.repository_id.as_ref(), Some(&forge.repo.id));
    assert_eq!(space.mode, Some("worker".into()));
    assert_eq!(forge.related(&launched).await, [forge.run(1)]);
    assert_eq!(forge.started_run(&launched).await, Some(forge.run(1)));
}

#[tokio::test]
async fn a_run_on_a_tag_starts_a_chat_in_the_mode_named() {
    let forge = Forge::new().await;

    let launched = forge.start(2, |l| l.mode = Some("plan".into()), None);
    let launched = launched.await.unwrap();

    assert_eq!(launched.space.kind(), SpaceKind::Chat);
    assert_eq!(launched.space.name, "release.yml run 2 on v1.4.0");
    assert_eq!(launched.space.mode, Some("plan".into()));
}

#[tokio::test]
async fn a_run_on_a_branch_opens_its_workspace() {
    let forge = Forge::new().await;

    let launched = forge.start(3, |_| {}, None).await.unwrap();

    let workspace = launched.space.clone().into_workspace().unwrap();
    assert_eq!(
        workspace.branch().map(|branch| branch.as_str()),
        Some("fix")
    );
    assert_eq!(forge.related(&launched).await, [forge.run(3)]);
    assert_eq!(forge.started_run(&launched).await, Some(forge.run(3)));
}

#[tokio::test]
async fn a_chat_mode_starts_a_chat_on_a_branch_run() {
    let forge = Forge::new().await;

    let launched = forge.start(3, |l| l.mode = Some("chat".into()), None);
    let launched = launched.await.unwrap();

    assert_eq!(launched.space.kind(), SpaceKind::Chat);
    assert_eq!(forge.related(&launched).await, [forge.run(3)]);
}

#[tokio::test]
async fn a_pr_run_reopens_the_workspace_of_the_prs_branch() {
    let forge = Forge::new().await;
    let topic = forge
        .manager
        .create_workspace(&forge.repo.id, "topic".into(), None, None, None);
    let topic = topic.await.unwrap();

    let launched = forge.start(4, |_| {}, None).await.unwrap();

    assert_eq!(launched.space.id, topic.id);
    assert_eq!(forge.related(&launched).await, [forge.run(4)]);
    assert_eq!(forge.started_run(&launched).await, Some(forge.run(4)));
}

#[tokio::test]
async fn a_run_beside_another_repository_is_refused() {
    let forge = Forge::new().await;
    let (temp, other) = (tempfile::tempdir().unwrap(), "other");
    let path = repository(temp.path(), other).to_str().unwrap().into();
    let other = forge.manager.repositories.register(path, None, None);
    let other = other.await.unwrap();

    let refused = forge.start(1, |l| l.repository = Some(other.id.to_string()), None);

    assert!(refused.await.is_err());
    assert!(
        forge
            .manager
            .spaces
            .list_workspaces()
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_scoped_start_on_another_repositorys_run_is_refused() {
    let forge = Forge::new().await;
    let temp = tempfile::tempdir().unwrap();
    let path = repository(temp.path(), "other").to_str().unwrap().into();
    let other = forge.manager.repositories.register(path, None, None);
    let other = other.await.unwrap();
    let parent = chat(&forge.manager, "parent", "chat", Some(&other), None).await;
    let caller = Caller {
        space_id: parent.id.clone(),
        execution: None,
        granted: None,
        access: Access::Full,
        vouched: None,
    };

    let refused = forge.start(1, |_| {}, Some(&caller)).await;

    let refused = format!("{:#}", refused.unwrap_err());
    assert!(refused.contains("own repository"), "{refused}");
}

#[tokio::test]
async fn ci_names_the_run_as_a_positional_link_does() {
    let forge = Forge::new().await;
    let flagged = |launch: &mut Launch| {
        let run = launch.target.take().unwrap();
        let StartTarget::Run(run) = run else {
            panic!("not a run: {run:?}");
        };
        launch.ci = Some(ItemChoice::Named(run.into()));
    };

    let launched = forge.start(1, flagged, None).await.unwrap();

    assert_eq!(launched.space.kind(), SpaceKind::Chat);
    assert_eq!(forge.related(&launched).await, [forge.run(1)]);
}

#[tokio::test]
async fn a_creation_on_a_branch_run_links_it_and_one_on_the_default_branch_is_refused() {
    let forge = Forge::new().await;

    let created = forge.create(3).await.unwrap();
    let refused = forge.create(1).await;

    let workspace = &created.workspace;
    assert_eq!(
        workspace.branch().map(|branch| branch.as_str()),
        Some("fix")
    );
    let space = SpaceSelector::id(&workspace.id);
    let space = forge.manager.spaces.space(&space).await.unwrap();
    let related: Vec<_> = (space.links.related.iter())
        .map(|item| item.url.to_string())
        .collect();
    assert_eq!(related, [forge.run(3)]);
    assert!(refused.is_err());
}

#[tokio::test]
async fn ci_names_a_run_by_its_number_in_the_repository_the_start_names() {
    let forge = Forge::new().await;
    let numbered = |repository: Option<String>| {
        move |launch: &mut Launch| {
            launch.target = None;
            launch.ci = Some(ItemChoice::Named(ItemInput::Number(1)));
            launch.repository = repository;
        }
    };

    let launched = forge.start(1, numbered(Some(forge.repo.id.to_string())), None);
    let launched = launched.await.unwrap();
    let unplaced = forge.start(1, numbered(None), None).await;

    assert_eq!(launched.space.name, "release.yml run 1 on main");
    assert_eq!(forge.related(&launched).await, [forge.run(1)]);
    assert!(unplaced.is_err());
}

#[tokio::test]
async fn a_chat_without_a_repository_starts_on_a_run_by_its_number_with_repo() {
    let forge = Forge::new().await;
    let master = chat(&forge.manager, "master", "orchestrate", None, None).await;
    let caller = Caller {
        space_id: master.id.clone(),
        execution: None,
        granted: None,
        access: Access::Full,
        vouched: None,
    };
    let numbered = |launch: &mut Launch| {
        launch.target = None;
        launch.ci = Some(ItemChoice::Named(ItemInput::Number(1)));
        launch.repository = Some(forge.repo.id.to_string());
    };

    let launched = forge.start(1, numbered, Some(&caller)).await.unwrap();

    assert_eq!(launched.space.repository_id.as_ref(), Some(&forge.repo.id));
    assert_eq!(forge.related(&launched).await, [forge.run(1)]);
}
