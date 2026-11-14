use std::{fs, path::Path, sync::Arc};

use super::*;
use crate::client::space_ids::SpaceId;
use crate::{
    daemon::{
        scope::ScopedExecution,
        test_support::{commit, git, manager, repository},
    },
    protocol::WorkspaceMethod,
};

struct Fixture {
    _root: tempfile::TempDir,
    manager: Arc<Manager>,
    repo: Repository,
}

impl Fixture {
    async fn new() -> Self {
        let (root, manager) = manager().await;
        let checkout = repository(root.path(), "repo");
        Self::registered(root, manager, &checkout).await
    }

    /// A fixture whose registered checkout is a clone of `origin`.
    async fn cloned() -> (Self, std::path::PathBuf) {
        let (root, manager) = manager().await;
        let origin = repository(root.path(), "origin");
        git(
            root.path(),
            &["clone", "-q", origin.to_str().unwrap(), "clone"],
        );
        let clone = root.path().join("clone");
        (Self::registered(root, manager, &clone).await, origin)
    }

    async fn registered(root: tempfile::TempDir, manager: Arc<Manager>, checkout: &Path) -> Self {
        let repo = manager
            .register_repository(checkout.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        Self {
            _root: root,
            manager,
            repo,
        }
    }

    async fn workspace(&self, name: &str, base: Option<&str>) -> Workspace {
        (self.manager)
            .create_workspace(&self.repo.id, name.into(), base.map(Into::into), None, None)
            .await
            .unwrap()
    }

    async fn refresh(&self, workspace: &Workspace) -> Result<Refreshed> {
        self.refresh_as(workspace, None).await
    }

    async fn refresh_as(
        &self,
        workspace: &Workspace,
        caller: Option<&Caller>,
    ) -> Result<Refreshed> {
        (self.manager)
            .refresh_workspace(&SpaceSelector::id(&workspace.id), caller)
            .await
    }

    async fn state(&self, workspace: &Workspace) -> WorkspaceState {
        (self.manager.workspace_by_id(&workspace.id).await)
            .unwrap()
            .state
    }
}

fn head(path: &Path) -> String {
    git(path, &["rev-parse", "HEAD"]).trim().to_owned()
}

/// Commit a new file `name` at `path`, returning the commit.
fn add_commit(path: &Path, name: &str) -> String {
    fs::write(path.join(name), format!("{name}\n")).unwrap();
    commit(path, name);
    head(path)
}

#[tokio::test]
async fn a_branch_workspace_fast_forwards_to_the_default_branch_once() {
    let f = Fixture::new().await;
    let workspace = f.workspace("topic", None).await;
    let start = head(&workspace.path);
    let main = add_commit(&f.repo.path, "later");

    let refreshed = f.refresh(&workspace).await.unwrap();
    assert_eq!(
        (
            refreshed.from.as_str(),
            refreshed.to.as_str(),
            refreshed.up_to_date
        ),
        (start.as_str(), main.as_str(), false)
    );
    assert_eq!(refreshed.target, "main");
    assert_eq!(head(&workspace.path), main);
    assert_eq!(f.state(&workspace).await, WorkspaceState::Ready);

    let again = f.refresh(&workspace).await.unwrap();
    assert!(again.up_to_date);
    assert_eq!(again.to, main);
}

#[tokio::test]
async fn a_stacked_workspace_fast_forwards_to_its_base_workspaces_branch() {
    let f = Fixture::new().await;
    let base = f.workspace("base", None).await;
    let child = f.workspace("child", Some("base")).await;
    assert!(child.base_workspace.is_some());
    let tip = add_commit(&base.path, "base-work");
    add_commit(&f.repo.path, "main-work");

    let refreshed = f.refresh(&child).await.unwrap();
    assert_eq!(refreshed.target, "base");
    assert_eq!(head(&child.path), tip);
}

#[tokio::test]
async fn a_branch_with_an_upstream_fast_forwards_to_it() {
    let (f, origin) = Fixture::cloned().await;
    let workspace = f.workspace("topic", None).await;
    git(&workspace.path, &["push", "-q", "-u", "origin", "topic"]);
    git(&origin, &["switch", "-q", "topic"]);
    let pushed = add_commit(&origin, "pushed");
    git(&origin, &["switch", "-q", "main"]);
    add_commit(&origin, "main-work");

    let refreshed = f.refresh(&workspace).await.unwrap();
    assert_eq!(refreshed.target, "origin/topic");
    assert_eq!(head(&workspace.path), pushed);
}

#[tokio::test]
async fn commits_and_changes_of_its_own_are_refused_without_moving_it() {
    let f = Fixture::new().await;
    let workspace = f.workspace("topic", None).await;
    let own = add_commit(&workspace.path, "own");
    add_commit(&f.repo.path, "later");

    let refused = f.refresh(&workspace).await.unwrap_err().to_string();
    assert!(refused.contains("1 commit(s)"), "{refused}");
    assert!(refused.contains("git rebase main"), "{refused}");
    assert_eq!(head(&workspace.path), own);
    assert_eq!(f.state(&workspace).await, WorkspaceState::Ready);

    git(&workspace.path, &["reset", "-q", "--hard", "HEAD~1"]);
    fs::write(workspace.path.join("unsaved"), "unsaved\n").unwrap();
    let refused = f.refresh(&workspace).await.unwrap_err().to_string();
    assert!(
        refused.contains("uncommitted changes to unsaved"),
        "{refused}"
    );
    assert!(!refused.contains("commit(s)"), "{refused}");
}

#[tokio::test]
async fn an_unchanged_base_leaves_a_workspace_with_its_own_work_up_to_date() {
    let f = Fixture::new().await;
    let workspace = f.workspace("topic", None).await;
    let own = add_commit(&workspace.path, "own");
    fs::write(workspace.path.join("unsaved"), "unsaved\n").unwrap();

    let refreshed = f.refresh(&workspace).await.unwrap();
    assert!(refreshed.up_to_date);
    assert_eq!(head(&workspace.path), own);
}

#[tokio::test]
async fn another_execution_refuses_a_refresh_and_the_callers_own_does_not() {
    let f = Fixture::new().await;
    let workspace = f.workspace("topic", None).await;
    let main = add_commit(&f.repo.path, "later");
    let id = workspace.id.clone();
    (f.manager.store)
        .run(move |db| {
            db.execute(
                "INSERT INTO executions(id,space_id,state) VALUES ('agent',?1,'running')",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let refused = f.refresh(&workspace).await.unwrap_err().to_string();
    assert!(refused.contains("shoal stop"), "{refused}");
    assert_eq!(f.state(&workspace).await, WorkspaceState::Ready);

    let own = caller(&workspace.id, "agent");
    f.refresh_as(&workspace, Some(&own)).await.unwrap();
    assert_eq!(head(&workspace.path), main);
}

#[tokio::test]
async fn workspace_processes_refresh_only_their_own_tree() {
    let f = Fixture::new().await;
    let own = f.workspace("own", None).await;
    let other = f.workspace("other", None).await;
    (f.manager.executions)
        .issue_scope("scope".into(), caller(&own.id, "execution"))
        .await;
    for (workspace, allowed) in [(&own, true), (&other, false)] {
        let mut request = WorkspaceMethod::RefreshWorkspace {
            workspace: SpaceSelector::id(&workspace.id),
        };
        let authorized =
            crate::daemon::scope::authorize(&f.manager, Some(&"scope".into()), &mut request).await;
        assert_eq!(authorized.is_ok(), allowed, "{}", workspace.name);
    }
}

#[tokio::test]
async fn a_chat_has_nothing_to_refresh() {
    let f = Fixture::new().await;
    let chat =
        crate::daemon::test_support::chat(&f.manager, "talk", "chat", Some(&f.repo), None).await;
    let refused = (f.manager)
        .refresh_workspace(&SpaceSelector::id(&chat.id), None)
        .await
        .unwrap_err();
    assert!(refused.to_string().contains("a chat"), "{refused}");
}

/// The scoped execution `execution` of the space `space`.
fn caller(space: &SpaceId, execution: &str) -> Caller {
    Caller {
        space_id: space.clone(),
        execution: Some(ScopedExecution {
            id: execution.into(),
            kind: crate::daemon::workspace::ExecutionKind::Command,
            agent_execution: None,
        }),
    }
}
