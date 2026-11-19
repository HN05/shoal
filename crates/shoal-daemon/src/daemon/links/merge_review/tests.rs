use std::sync::Arc;

use super::*;
use crate::{
    daemon::{
        scope::Scoped,
        test_support::{configure, git, manager, repository},
    },
    forge::items::action::MergeMethod,
    protocol::{ItemContext, ItemMethod},
    reviews::{ReviewTarget, output::ReviewerOutput},
};
use shoal_protocol::{space_names::SpaceSelector, time::UnixTime};
use shoal_reviews::store;

/// A workspace on `topic`, whose forge serves PR 1 from `head_branch` at
/// the workspace's HEAD, in a repository whose `require_for_merge` is
/// `holds`.
struct Fixture {
    _root: tempfile::TempDir,
    _server: shoal_forge::test_support::Server,
    manager: Arc<Manager>,
    workspace: Workspace,
    head: CommitId,
}

impl Fixture {
    async fn new(holds: &str, head_branch: &str) -> Self {
        let (root, manager) = manager().await;
        let repo = repository(root.path(), "repo");
        let repo_id = (manager.register_repository(repo.to_str().unwrap().into(), None, None))
            .await
            .unwrap()
            .id;
        let workspace = (manager.create_workspace(&repo_id, "topic".into(), None, None, None))
            .await
            .unwrap();
        let head = crate::daemon::links::pr::current_head(&workspace)
            .await
            .unwrap();
        let (sha, branch) = (head.to_string(), head_branch.to_owned());
        let server = shoal_forge::test_support::Server::start(move |_| {
            let pull = serde_json::json!({
                "number": 1, "title": "Fix", "state": "open", "merged": false,
                "draft": false,
                "head": {"ref": branch, "sha": sha, "repo": {"full_name": "team/repo"}},
                "base": {"ref": "main"}, "labels": [],
            });
            (200, Vec::new(), pull.to_string().into())
        });
        let host = &server.host;
        crate::forge::hosts::learn(host, crate::forge::backends::ForgeKind::Forgejo);
        configure(
            &manager,
            &format!(
                "[review]\nrequire_for_merge = '{holds}'\n\
                 [machine.forges.\"{host}\"]\ntype = \"forgejo\"\nuser_token = \"user\"\n"
            ),
        )
        .await;
        git(
            &repo,
            &[
                "remote",
                "add",
                "origin",
                &format!("http://{host}/team/repo"),
            ],
        );
        Self {
            _root: root,
            _server: server,
            manager,
            workspace,
            head,
        }
    }

    fn context(&self) -> RepoTarget {
        ItemContext::space(SpaceSelector::id(&self.workspace.id)).into()
    }

    fn agent(&self) -> Caller {
        Caller {
            space_id: self.workspace.id.clone(),
            execution: None,
            granted: None,
            read_only: false,
            vouched: None,
        }
    }

    /// Gate a merge of PR 1 that names `head` for `caller`, returning the
    /// head it then names.
    async fn gate(
        &self,
        head: Option<CommitId>,
        caller: Option<&Caller>,
    ) -> Result<Option<CommitId>> {
        let mut merge = merge(head);
        let pull = Some("1".parse().unwrap());
        (self.manager)
            .gate_pull_merge(&self.context(), pull, &mut merge, caller)
            .await?;
        Ok(merge.merge_head().cloned())
    }

    /// Record a finished review of the workspace at its HEAD that found
    /// nothing.
    async fn reviewed(&self) {
        let (id, head) = (self.workspace.id.clone(), self.head.clone());
        let write = move |tx: &rusqlite::Transaction<'_>| {
            let at = UnixTime::from_seconds;
            let target = ReviewTarget::Workspace;
            let review = store::queue(tx, &id, &target, &"fake".into(), &Default::default(), at(1));
            let review = review?.id();
            store::start(tx, review, &head, &"base".into(), at(2))?;
            let output: ReviewerOutput = serde_json::from_value(serde_json::json!({
                "summary": "Fine.", "verdict": "approve", "checked": [], "findings": []
            }))?;
            store::finish(tx, review, &"fake".into(), &output, None, at(3))?;
            Ok(())
        };
        self.manager.store.write(write).await.unwrap();
    }
}

fn merge(head: Option<CommitId>) -> Action {
    Action::Merge {
        method: Some(MergeMethod::Merge),
        message: None,
        delete_branch: false,
        head,
    }
}

#[tokio::test]
async fn an_agents_pr_merge_waits_for_its_owners_review_of_the_head_it_names() {
    let f = Fixture::new("agents", "topic").await;
    // "agents" leaves the user's merge, and the head it names, alone.
    assert_eq!(f.gate(None, None).await.unwrap(), None);
    let agent = f.agent();
    let refused = f.gate(None, Some(&agent)).await.unwrap_err().to_string();
    assert!(refused.contains(f.head.short()), "{refused}");

    f.reviewed().await;
    let bound = f.gate(None, Some(&agent)).await.unwrap();
    assert_eq!(bound, Some(f.head.clone()));
    // The review covers only the head it reviewed.
    let other = CommitId::from("b".repeat(40));
    f.gate(Some(other), Some(&agent)).await.unwrap_err();
}

#[tokio::test]
async fn a_refused_pr_merge_asks_for_no_approval() {
    let f = Fixture::new("agents", "topic").await;
    let mut method = ItemMethod::ItemAction {
        target: f.context(),
        kind: ItemKind::Pr,
        item: Some("1".parse().unwrap()),
        action: merge(None),
        confirmed: None,
    };
    let scoped = method.scope(&f.manager, &f.agent()).await.map(drop);
    let refused = scoped.unwrap_err().to_string();
    assert!(refused.contains(f.head.short()), "{refused}");
    let selector = SpaceSelector::id(&f.workspace.id);
    let requests = f.manager.approval_requests(Some(&selector)).await.unwrap();
    assert!(requests.is_empty(), "{requests:?}");
}

#[tokio::test]
async fn all_holds_the_users_pr_merge_and_binds_it_to_the_reviewed_head() {
    let f = Fixture::new("all", "topic").await;
    let context = f.context();
    let pull = || Some("1".parse().unwrap());
    // The warning the question shows comes after the gate too.
    let warning = f
        .manager
        .require_pull_review(&context, pull(), None, None, None);
    warning.await.unwrap_err();
    f.gate(None, None).await.unwrap_err();
    f.reviewed().await;
    assert_eq!(f.gate(None, None).await.unwrap(), Some(f.head.clone()));
}

#[tokio::test]
async fn a_pr_whose_branch_no_workspace_owns_is_refused() {
    let f = Fixture::new("agents", "elsewhere").await;
    let refused = f.gate(None, Some(&f.agent())).await.unwrap_err();
    let refused = refused.to_string();
    assert!(refused.contains("elsewhere"), "{refused}");
}

#[tokio::test]
async fn a_branchs_own_config_does_not_turn_its_merge_gate_off() {
    let f = Fixture::new("agents", "topic").await;
    let config = "[review]\nrequire_for_merge = 'off'\n";
    std::fs::write(f.workspace.path.join(".shoal.toml"), config).unwrap();
    f.gate(None, Some(&f.agent())).await.unwrap_err();
}

#[tokio::test]
async fn a_review_of_another_head_is_left_to_shoal_review_run() {
    let f = Fixture::new("agents", "topic").await;
    let other = CommitId::from("b".repeat(40));
    let refused = f.gate(Some(other), Some(&f.agent())).await.unwrap_err();
    let refused = refused.to_string();
    assert!(refused.contains("shoal review run"), "{refused}");
}

#[tokio::test]
async fn a_caller_outside_the_owners_tree_queues_no_review_there() {
    let f = Fixture::new("agents", "topic").await;
    let repository = f.workspace.repository().clone();
    let sibling =
        f.manager
            .create_workspace(&repository, "other".into(), Some("main".into()), None, None);
    let sibling = sibling.await.unwrap();
    let caller = Caller::of_space(sibling.id.clone());
    let mut merge = merge(None);
    let context = ItemContext::space(SpaceSelector::id(&sibling.id)).into();
    let gated = f.manager.gate_pull_merge(
        &context,
        Some("1".parse().unwrap()),
        &mut merge,
        Some(&caller),
    );
    let refused = gated.await.unwrap_err().to_string();
    assert!(refused.contains("shoal review run"), "{refused}");
    let selector = SpaceSelector::id(&f.workspace.id);
    assert!(
        f.manager
            .reviews
            .latest(f.manager.as_ref(), &selector)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_held_auto_merge_needs_a_forge_that_keeps_it_at_the_reviewed_head() {
    let f = Fixture::new("agents", "topic").await;
    f.reviewed().await;
    let mut auto = Action::AutoMerge {
        method: Some(MergeMethod::Merge),
        message: None,
        delete_branch: false,
        head: None,
    };
    let (context, agent) = (f.context(), f.agent());
    let pull = || Some("1".parse().unwrap());
    let gated = f
        .manager
        .gate_pull_merge(&context, pull(), &mut auto, Some(&agent));
    // Forgejo's automatic merge merges whatever head the PR has later.
    gated.await.unwrap_err();
    // The user's, which the gate does not hold, goes ahead.
    let gated = f.manager.gate_pull_merge(&context, pull(), &mut auto, None);
    gated.await.unwrap();
}

#[tokio::test]
async fn the_global_config_holds_a_pr_of_an_unregistered_repository() {
    let f = Fixture::new("all", "topic").await;
    let host = &f._server.host;
    let elsewhere: ItemInput = format!("http://{host}/team/other/pulls/1").parse().unwrap();
    let context = f.context();
    let gated = f
        .manager
        .require_pull_review(&context, Some(elsewhere), None, None, None);
    let refused = gated.await.unwrap_err().to_string();
    assert!(refused.contains("no workspace owns"), "{refused}");
}
