use std::sync::Arc;

use super::*;
use crate::test_support::{git, manager, repository};

struct Fixture {
    _root: tempfile::TempDir,
    manager: Arc<Manager>,
    workspace: Workspace,
    /// The commit `origin/topic` names, one ahead of the workspace.
    pushed: String,
}

impl Fixture {
    async fn new() -> Self {
        let (root, manager) = manager().await;
        let checkout = repository(root.path(), "repo");
        let repo = manager
            .register_repository(checkout.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "topic".into(), None, None, None)
            .await
            .unwrap();
        let path = &workspace.path;
        git(path, &["commit", "--allow-empty", "-m", "pushed"]);
        let pushed = git(path, &["rev-parse", "HEAD"]).trim().to_owned();
        git(path, &["update-ref", "refs/remotes/origin/topic", &pushed]);
        git(path, &["reset", "--hard", "HEAD~1"]);
        Self {
            _root: root,
            manager,
            workspace,
            pushed,
        }
    }

    fn head(&self, reference: &str) -> String {
        git(&self.workspace.path, &["rev-parse", reference])
            .trim()
            .to_owned()
    }

    async fn state(&self) -> WorkspaceState {
        self.manager.workspace("topic").await.unwrap().state
    }
}

#[tokio::test]
async fn a_workspace_behind_its_pushed_branch_fast_forwards_once() {
    let f = Fixture::new().await;
    assert!(
        f.manager
            .fast_forward_workspace("topic", "topic")
            .await
            .unwrap()
    );
    assert_eq!(f.head("topic"), f.pushed);
    assert!(
        !f.manager
            .fast_forward_workspace("topic", "topic")
            .await
            .unwrap()
    );
    assert_eq!(f.state().await, WorkspaceState::Ready);
}

#[tokio::test]
async fn a_switched_workspace_is_refused_without_moving_its_branch() {
    let f = Fixture::new().await;
    let base = f.head("HEAD");
    git(&f.workspace.path, &["switch", "-c", "scratch"]);
    let error = f
        .manager
        .fast_forward_workspace("topic", "topic")
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("no longer on its recorded branch"),
        "{error:#}"
    );
    assert_eq!(f.head("scratch"), base, "the checked-out branch stays");
    assert_eq!(f.state().await, WorkspaceState::Ready);

    git(&f.workspace.path, &["switch", "topic"]);
    let error = f
        .manager
        .fast_forward_workspace("topic", "other")
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("recorded on topic"),
        "{error:#}"
    );
    assert_eq!(f.head("topic"), base);
}
