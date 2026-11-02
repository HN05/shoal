use std::path::Path;

use crate::{
    daemon::scope::{Caller, ScopedExecution},
    model::{MadeFor, Workspace},
    protocol::{CommitStart, WorkspaceMethod},
    state::WorkspaceState,
    test_support::{commit, git, manager, repository},
};

async fn create(
    manager: &super::Manager,
    repo: &str,
    parent: Option<&str>,
    start: CommitStart,
) -> anyhow::Result<Workspace> {
    manager
        .create_commit_workspace(repo.into(), parent.map(str::to_owned), start, None)
        .await
}

fn head(path: &Path) -> (String, String) {
    (
        git(path, &["rev-parse", "HEAD"]).trim().to_owned(),
        git(path, &["branch", "--show-current"]).trim().to_owned(),
    )
}

#[tokio::test]
async fn a_workspace_at_a_commit_has_no_branch_and_is_named_after_its_start() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let first = git(&checkout, &["rev-parse", "HEAD"]).trim().to_owned();
    git(&checkout, &["tag", "v1.4.0"]);
    std::fs::write(checkout.join("later"), "later\n").unwrap();
    commit(&checkout, "later");
    let main = git(&checkout, &["rev-parse", "HEAD"]).trim().to_owned();
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();

    let at_main = create(&manager, &repo.id, None, CommitStart::Revision(None))
        .await
        .unwrap();
    assert_eq!(at_main.branch, None);
    assert_eq!(at_main.state, WorkspaceState::Ready);
    assert_eq!(at_main.name, format!("main-{}", &main[..7]));
    let at = at_main.at.clone().unwrap();
    assert_eq!(
        (at.commit.as_str(), at.made_for),
        (main.as_str(), Some(MadeFor::Ref("main".into())))
    );
    assert_eq!(head(&at_main.path), (main.clone(), String::new()));
    // Its diff base is where it started.
    assert_eq!(manager.diff_base(&at_main.id).await.unwrap().commit, main);

    let again = create(&manager, &repo.id, None, CommitStart::Revision(None))
        .await
        .unwrap();
    assert_eq!(again.name, format!("{}-2", at_main.name));

    let tagged = create(
        &manager,
        &repo.id,
        None,
        CommitStart::Revision(Some("v1.4.0".into())),
    )
    .await
    .unwrap();
    assert_eq!(tagged.name, "v1-4-0");
    assert_eq!(
        tagged.at.unwrap().made_for,
        Some(MadeFor::Ref("v1.4.0".into()))
    );
    assert_eq!(head(&tagged.path).0, first);

    let bare = create(
        &manager,
        &repo.id,
        None,
        CommitStart::Revision(Some(first.clone())),
    )
    .await
    .unwrap();
    assert_eq!(bare.name, first[..7]);
    assert_eq!(bare.at.unwrap().made_for, None);
}

#[tokio::test]
async fn a_tag_only_origin_has_is_fetched_with_its_tags() {
    let (temp, manager) = manager().await;
    let origin = repository(temp.path(), "origin");
    let checkout = temp.path().join("clone");
    git(
        temp.path(),
        &["clone", "-q", origin.to_str().unwrap(), "clone"],
    );
    std::fs::write(origin.join("released"), "1\n").unwrap();
    commit(&origin, "released");
    git(&origin, &["tag", "release"]);
    let released = git(&origin, &["rev-parse", "HEAD"]).trim().to_owned();
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = create(
        &manager,
        &repo.id,
        None,
        CommitStart::Revision(Some("release".into())),
    )
    .await
    .unwrap();
    assert_eq!(head(&workspace.path).0, released);
    assert!(
        create(
            &manager,
            &repo.id,
            None,
            CommitStart::Revision(Some("missing".into()))
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn a_pr_head_is_fetched_from_the_ref_its_forge_serves() {
    let (temp, manager) = manager().await;
    let origin = repository(temp.path(), "origin");
    git(
        temp.path(),
        &["clone", "-q", origin.to_str().unwrap(), "clone"],
    );
    std::fs::write(origin.join("fork"), "fork\n").unwrap();
    commit(&origin, "fork");
    let fork = git(&origin, &["rev-parse", "HEAD"]).trim().to_owned();
    git(&origin, &["update-ref", "refs/pull/812/head", &fork]);
    git(&origin, &["reset", "-q", "--hard", "HEAD~1"]);
    let repo = manager
        .register_repository(
            temp.path().join("clone").to_str().unwrap().into(),
            None,
            None,
        )
        .await
        .unwrap();
    let pull = |commit: &str| CommitStart::PullHead {
        number: 812,
        reference: "refs/pull/812/head".into(),
        commit: commit.into(),
    };
    let moved = create(&manager, &repo.id, None, pull(&"0".repeat(40))).await;
    assert!(moved.unwrap_err().to_string().contains("moved"));
    let workspace = create(&manager, &repo.id, None, pull(&fork)).await.unwrap();
    assert_eq!(workspace.name, "pr-812");
    assert_eq!(workspace.at.unwrap().made_for, Some(MadeFor::Pr(812)));
    assert_eq!(head(&workspace.path), (fork, String::new()));
    // No ref keeps the fetched head; the worktree's HEAD does.
    assert!(git(&repo.path, &["for-each-ref", "refs/shoal"]).is_empty());
}

#[tokio::test]
async fn a_child_at_its_parents_branch_starts_at_the_parents_commit_without_stacking() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    std::fs::write(parent.path.join("work"), "work\n").unwrap();
    commit(&parent.path, "work");
    let child = create(
        &manager,
        &repo.id,
        Some(&parent.id),
        CommitStart::Revision(Some("parent".into())),
    )
    .await
    .unwrap();
    assert_eq!(head(&child.path).0, head(&parent.path).0);
    assert_eq!(
        child.parent_workspace.map(|parent| parent.id),
        Some(parent.id.clone())
    );
    assert_eq!(child.base_workspace, None);
    // The parent's own worktree stays on its branch.
    assert_eq!(head(&parent.path).1, "parent");
}

#[tokio::test]
async fn workspace_processes_create_workspaces_at_a_commit_only_below_their_own() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let create_branch =
        |name: &str| manager.create_workspace(&repo.id, name.into(), None, None, None);
    let own = create_branch("own").await.unwrap();
    let other = create_branch("other").await.unwrap();
    manager
        .executions
        .issue_scope(
            "scope".into(),
            Caller {
                workspace_id: own.id.clone(),
                execution: Some(ScopedExecution {
                    id: "execution".into(),
                    kind: super::super::ExecutionKind::Command,
                    agent: None,
                }),
            },
        )
        .await;
    for (parent, allowed) in [
        (Some(own.name.clone()), true),
        (Some(other.name.clone()), false),
        (None, false),
    ] {
        let mut request = WorkspaceMethod::CreateCommitWorkspace {
            repository: repo.id.as_str().into(),
            parent: parent.clone(),
            start: CommitStart::Revision(None),
            chat: None,
        };
        let authorized =
            crate::daemon::scope::authorize(&manager, Some("scope"), &mut request).await;
        assert_eq!(authorized.is_ok(), allowed, "{parent:?}");
    }
}
