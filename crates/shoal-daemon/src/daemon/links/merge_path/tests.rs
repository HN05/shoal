use std::sync::Arc;

use anyhow::Result;
use shoal_protocol::space_names::SpaceSelector;

use crate::{
    daemon::{
        manager::Manager,
        test_support::{configure, git, manager, repository},
    },
    model::{MergePath, Workspace},
};

/// A workspace on `topic` whose repository's forge serves PR 1 in `state`,
/// linked to it as assigned.
struct Fixture {
    _root: tempfile::TempDir,
    _server: shoal_forge::test_support::Server,
    manager: Arc<Manager>,
    workspace: Workspace,
    repo: std::path::PathBuf,
    host: String,
}

impl Fixture {
    async fn new(state: &'static str) -> Self {
        let (root, manager) = manager().await;
        let repo = repository(root.path(), "repo");
        let repo_id = (manager
            .repositories
            .register(repo.to_str().unwrap().into(), None, None))
        .await
        .unwrap()
        .id;
        let workspace = (manager.create_workspace(&repo_id, "topic".into(), None, None, None))
            .await
            .unwrap();
        let server = shoal_forge::test_support::Server::start(move |_| {
            let pull = serde_json::json!({
                "number": 1, "title": "Fix", "state": state, "merged": false,
                "draft": false,
                "head": {"ref": "topic", "sha": "c0ffee", "repo": {"full_name": "team/repo"}},
                "base": {"ref": "main"}, "labels": [],
            });
            (200, Vec::new(), pull.to_string().into())
        });
        let host = server.host.clone();
        crate::forge::hosts::learn(&host, crate::forge::backends::ForgeKind::Forgejo);
        configure(
            &manager,
            &format!(
                "[machine.forges.\"{host}\"]\ntype = \"forgejo\"\n\
                 user_token = \"user\"\nagent_token = \"agent\"\n"
            ),
        )
        .await;
        let origin = format!("http://{host}/team/repo");
        git(&repo, &["remote", "add", "origin", &origin]);
        let f = Self {
            _root: root,
            _server: server,
            manager,
            workspace,
            repo,
            host,
        };
        f.link(&format!("{origin}/pulls/1")).await;
        f
    }

    /// Link the PR at `url` to the workspace as assigned.
    async fn link(&self, url: &str) {
        let (id, record) = (
            self.workspace.id.clone(),
            format!(r#"{{"urls":["{url}"]}}"#),
        );
        let link = move |tx: &rusqlite::Transaction<'_>| {
            tx.execute(
                "INSERT INTO pr_cleanup(space_id,record) VALUES (?1,?2)",
                [id.as_str(), record.as_str()],
            )?;
            Ok(())
        };
        self.manager.store.write(link).await.unwrap();
    }

    async fn path(&self) -> Result<MergePath> {
        let selector = SpaceSelector::id(&self.workspace.id);
        self.manager
            .links
            .merge_path(&*self.manager, &selector)
            .await
    }
}

#[tokio::test]
async fn an_open_linked_pr_takes_the_merge_and_refuses_a_local_one() {
    let f = Fixture::new("open").await;
    let Ok(MergePath::Pr { pr }) = f.path().await else {
        panic!("the open PR does not take the merge");
    };
    assert_eq!(
        pr.to_string(),
        format!("http://{}/team/repo/pulls/1", f.host)
    );
    // A local merge asked of the daemon directly is refused too.
    let selector = SpaceSelector::id(&f.workspace.id);
    let refused = f
        .manager
        .preview_merge(
            &selector,
            &crate::model::MergeOptions {
                push: Some(false),
                ..Default::default()
            },
            None,
        )
        .await;
    let refused = refused.unwrap_err().to_string();
    assert!(refused.contains("is open"), "{refused}");
}

#[tokio::test]
async fn a_closed_linked_pr_leaves_the_workspace_to_merge_locally() {
    let f = Fixture::new("closed").await;
    assert_eq!(f.path().await.unwrap(), MergePath::Local);
}

#[tokio::test]
async fn a_linked_pr_of_another_repository_than_origin_is_refused() {
    let f = Fixture::new("closed").await;
    let other = format!("http://{}/team/other", f.host);
    git(&f.repo, &["remote", "set-url", "origin", &other]);
    let refused = f.path().await.unwrap_err().to_string();
    assert!(refused.contains("different repository"), "{refused}");
}
