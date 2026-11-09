use crate::client::space_ids::SpaceId;
use crate::time::UnixTime;
use std::path::Path;

use crate::client::space_names::{SpaceName, SpaceSelector};
use crate::{
    daemon::{
        scope::{Caller, ScopedExecution},
        test_support::{commit, git, manager, repository},
    },
    model::{MadeFor, SpaceKind, Workspace},
    protocol::{CommitStart, WorkspaceMethod},
    state::WorkspaceState,
};

async fn create(
    manager: &super::Manager,
    repo: &str,
    parent: Option<&SpaceId>,
    start: CommitStart,
) -> anyhow::Result<Workspace> {
    manager
        .create_commit_workspace(repo.into(), parent.cloned(), start, Default::default())
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
    assert_eq!(
        manager
            .diff_base(&SpaceSelector::id(&at_main.id))
            .await
            .unwrap()
            .commit,
        main
    );

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
    // A short commit of only digits takes the kind as its prefix.
    assert_eq!(
        bare.name,
        SpaceName::derive(SpaceKind::Workspace, &first[..7])
    );
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
    // A branch origin deleted since the clone fetched it, and a local tag.
    git(&origin, &["branch", "gone"]);
    git(&checkout, &["fetch", "-q", "origin"]);
    git(&origin, &["branch", "-D", "gone"]);
    git(&checkout, &["tag", "local"]);
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
    // The fetch drops the stale tracking ref, which would count as pushed,
    // and keeps the local tag.
    assert!(git(&checkout, &["branch", "-r", "--list", "origin/gone"]).is_empty());
    assert!(!git(&checkout, &["tag", "--list", "local"]).is_empty());
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
    let pull = |commit: &str| {
        CommitStart::PullHead(crate::protocol::PullHead {
            number: 812,
            reference: "refs/pull/812/head".into(),
            commit: commit.into(),
            base: None,
        })
    };
    let moved = create(&manager, &repo.id, None, pull(&"0".repeat(40))).await;
    assert!(moved.is_err());
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
        child.parent.map(|parent| parent.id),
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
                space_id: own.id.clone(),
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
        // Without one it goes below the caller's own workspace.
        (None, true),
    ] {
        let mut request = WorkspaceMethod::CreateCommitWorkspace {
            repository: repo.id.as_str().into(),
            parent: parent.clone().map(SpaceSelector::from),
            start: CommitStart::Revision(None),
            options: Default::default(),
        };
        let authorized =
            crate::daemon::scope::authorize(&manager, Some("scope"), &mut request).await;
        assert_eq!(authorized.is_ok(), allowed, "{parent:?}");
        if parent.is_none() {
            let WorkspaceMethod::CreateCommitWorkspace { parent, .. } = request else {
                unreachable!()
            };
            assert_eq!(parent, Some(SpaceSelector::id(&own.id)));
        }
    }
}

#[tokio::test]
async fn its_starting_commit_counts_as_kept_and_later_commits_do_not() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    // A commit no remote or branch has, as a fork PR's head would be.
    git(&checkout, &["checkout", "-q", "--detach"]);
    std::fs::write(checkout.join("local"), "local\n").unwrap();
    commit(&checkout, "local");
    let local = git(&checkout, &["rev-parse", "HEAD"]).trim().to_owned();
    git(&checkout, &["checkout", "-q", "main"]);
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = create(&manager, &repo.id, None, CommitStart::Revision(Some(local)))
        .await
        .unwrap();
    let check = manager
        .check_removal(
            &SpaceSelector::id(&workspace.id),
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert!(check.safe() && check.unsaved_work().is_empty(), "{check:?}");
    assert!(matches!(
        manager.cleanup_reason(&workspace).await,
        crate::model::CleanupReason::Idle { .. }
    ));

    std::fs::write(workspace.path.join("later"), "later\n").unwrap();
    commit(&workspace.path, "later");
    let check = manager
        .check_removal(
            &SpaceSelector::id(&workspace.id),
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert_eq!(check.unpushed_commits, 1);
    assert_eq!(check.unsaved_work().len(), 1, "{check:?}");
}

#[tokio::test]
async fn a_removed_workspace_at_a_commit_is_restored_without_a_branch() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = create(&manager, &repo.id, None, CommitStart::Revision(None))
        .await
        .unwrap();
    std::fs::write(workspace.path.join("later"), "later\n").unwrap();
    commit(&workspace.path, "later");
    let removed_head = head(&workspace.path).0;
    std::fs::write(workspace.path.join("draft"), "draft\n").unwrap();
    manager
        .remove_workspace(
            &SpaceSelector::id(&workspace.id),
            crate::removal::UnsavedWork::All,
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert!(!workspace.path.exists());
    assert!(git(&checkout, &["branch", "--list", &workspace.name]).is_empty());

    let restored = manager
        .restore_workspace(&SpaceSelector::from(&workspace.name))
        .await
        .unwrap();
    let reopened = restored.opened.workspace;
    assert_eq!(reopened.branch, None);
    assert_eq!(reopened.at, workspace.at);
    assert_eq!(head(&reopened.path), (removed_head, String::new()));
    assert_eq!(
        std::fs::read_to_string(reopened.path.join("draft")).unwrap(),
        "draft\n"
    );
    assert!(git(&checkout, &["branch", "--list", &workspace.name]).is_empty());
}

#[tokio::test]
async fn a_prs_workspace_is_reused_and_moved_to_its_new_head_unless_it_has_work() {
    let (temp, manager) = manager().await;
    let origin = repository(temp.path(), "origin");
    git(
        temp.path(),
        &["clone", "-q", origin.to_str().unwrap(), "clone"],
    );
    let push_head = |file: &str| {
        std::fs::write(origin.join(file), "fork\n").unwrap();
        commit(&origin, file);
        let head = git(&origin, &["rev-parse", "HEAD"]).trim().to_owned();
        git(&origin, &["update-ref", "refs/pull/9/head", &head]);
        head
    };
    let repo = manager
        .register_repository(
            temp.path().join("clone").to_str().unwrap().into(),
            None,
            None,
        )
        .await
        .unwrap();
    let url = "https://forge.example/team/repo/pulls/9".to_owned();
    let open = |commit: String| {
        manager.open_pull_head(
            repo.id.as_str().into(),
            url.clone(),
            crate::protocol::PullHead {
                number: 9,
                reference: "refs/pull/9/head".into(),
                commit,
                base: Some("refs/remotes/origin/main".into()),
            },
            None,
            Default::default(),
            false,
        )
    };
    let first = push_head("first");
    let opened = open(first.clone()).await.unwrap();
    assert!(!opened.reused);
    // Its diff shows the PR's changes against the PR's base.
    let base = git(&repo.path, &["rev-parse", "origin/main"])
        .trim()
        .to_owned();
    assert_eq!(
        manager
            .diff_base(&SpaceSelector::id(&opened.workspace.id))
            .await
            .unwrap()
            .commit,
        base
    );
    // Reusing it counts as activity, which an idle period starts over from.
    let id = opened.workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute("UPDATE workspaces SET active_at=1 WHERE id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();
    let again = open(first).await.unwrap();
    assert!(again.reused);
    assert_eq!(again.workspace.id, opened.workspace.id);
    assert!(manager.active_at(&again.workspace.id).await.unwrap() > UnixTime::from_seconds(1));

    let second = push_head("second");
    let moved = open(second.clone()).await.unwrap();
    assert_eq!(moved.workspace.id, opened.workspace.id);
    assert_eq!(moved.workspace.at.unwrap().commit, second);
    assert_eq!(
        moved.workspace.base_commit.as_deref(),
        Some(second.as_str())
    );
    assert_eq!(head(&moved.workspace.path), (second, String::new()));

    std::fs::write(moved.workspace.path.join("own"), "own\n").unwrap();
    commit(&moved.workspace.path, "own");
    let third = push_head("third");
    open(third).await.unwrap_err();
}

#[tokio::test]
async fn a_workspace_at_a_commit_goes_below_the_chat_or_workspace_it_names() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(
            repo.id.clone().into(),
            "planning".into(),
            "claude".into(),
            std::process::id(),
            None,
            false,
        )
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let create = |parent: Option<SpaceId>| {
        manager.create_commit_workspace(
            repo.id.as_str().into(),
            parent,
            CommitStart::Revision(None),
            Default::default(),
        )
    };
    let below_chat = create(Some(chat.id.clone())).await.unwrap();
    assert_eq!(
        below_chat.parent.map(|parent| parent.id),
        Some(chat.id.clone())
    );
    let child = create(Some(parent.id.clone())).await.unwrap();
    assert_eq!(
        child.parent.map(|parent| parent.id),
        Some(parent.id.clone())
    );
    let top = create(None).await.unwrap();
    assert_eq!(top.parent, None);
}

#[tokio::test]
async fn removing_one_switched_onto_a_branch_leaves_that_branch() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    git(&checkout, &["switch", "-q", "-c", "topic"]);
    std::fs::write(checkout.join("topic"), "topic\n").unwrap();
    commit(&checkout, "topic");
    git(&checkout, &["switch", "-q", "main"]);
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = create(
        &manager,
        &repo.id,
        None,
        CommitStart::Revision(Some("topic".into())),
    )
    .await
    .unwrap();
    git(&workspace.path, &["switch", "-q", "topic"]);
    let check = manager
        .check_removal(
            &SpaceSelector::id(&workspace.id),
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    // On a branch, its starting commit no longer counts as kept.
    assert_eq!(check.unpushed_commits, 1, "{check:?}");
    let removed = manager
        .remove_workspace(
            &SpaceSelector::id(&workspace.id),
            crate::removal::UnsavedWork::All,
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert!(!removed.branch_outcome.is_deleted(), "{removed:?}");
    assert!(!git(&checkout, &["branch", "--list", "topic"]).is_empty());
    // Nor does cleanup delete it later as a branch the workspace kept.
    assert!(manager.list_kept_branches(None).await.unwrap().is_empty());
}

#[tokio::test]
async fn reopening_a_prs_workspace_records_the_prs_base() {
    let (temp, manager) = manager().await;
    let origin = repository(temp.path(), "origin");
    git(
        temp.path(),
        &["clone", "-q", origin.to_str().unwrap(), "clone"],
    );
    std::fs::write(origin.join("fork"), "fork\n").unwrap();
    commit(&origin, "fork");
    let head = git(&origin, &["rev-parse", "HEAD"]).trim().to_owned();
    git(&origin, &["update-ref", "refs/pull/5/head", &head]);
    let repo = manager
        .register_repository(
            temp.path().join("clone").to_str().unwrap().into(),
            None,
            None,
        )
        .await
        .unwrap();
    let open = |base: Option<&str>| {
        manager.open_pull_head(
            repo.id.as_str().into(),
            "https://forge.example/team/repo/pulls/5".into(),
            crate::protocol::PullHead {
                number: 5,
                reference: "refs/pull/5/head".into(),
                commit: head.clone(),
                base: base.map(str::to_owned),
            },
            None,
            Default::default(),
            false,
        )
    };
    let opened = open(None).await.unwrap();
    assert_eq!(opened.workspace.base_ref, None);
    let reopened = open(Some("refs/remotes/origin/main")).await.unwrap();
    assert!(reopened.reused);
    assert_eq!(
        reopened.workspace.base_ref.as_deref(),
        Some("refs/remotes/origin/main")
    );
    let main = git(&repo.path, &["rev-parse", "origin/main"])
        .trim()
        .to_owned();
    assert_eq!(
        manager
            .diff_base(&SpaceSelector::id(&opened.workspace.id))
            .await
            .unwrap()
            .commit,
        main
    );
}

#[tokio::test]
async fn a_restored_prs_workspace_drops_a_base_that_is_gone() {
    let (temp, manager) = manager().await;
    let origin = repository(temp.path(), "origin");
    git(
        temp.path(),
        &["clone", "-q", origin.to_str().unwrap(), "clone"],
    );
    std::fs::write(origin.join("fork"), "fork\n").unwrap();
    commit(&origin, "fork");
    let head = git(&origin, &["rev-parse", "HEAD"]).trim().to_owned();
    git(&origin, &["update-ref", "refs/pull/6/head", &head]);
    let clone = temp.path().join("clone");
    git(
        &clone,
        &["update-ref", "refs/remotes/origin/release", "main"],
    );
    let repo = manager
        .register_repository(clone.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let opened = manager
        .open_pull_head(
            repo.id.as_str().into(),
            "https://forge.example/team/repo/pulls/6".into(),
            crate::protocol::PullHead {
                number: 6,
                reference: "refs/pull/6/head".into(),
                commit: head.clone(),
                base: Some("refs/remotes/origin/release".into()),
            },
            None,
            Default::default(),
            false,
        )
        .await
        .unwrap();
    manager
        .remove_workspace(
            &SpaceSelector::id(&opened.workspace.id),
            crate::removal::UnsavedWork::All,
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    git(&clone, &["update-ref", "-d", "refs/remotes/origin/release"]);
    let restored = manager
        .restore_workspace(&SpaceSelector::from(&opened.workspace.name))
        .await
        .unwrap()
        .opened
        .workspace;
    assert_eq!(restored.base_ref, None);
    assert_eq!(
        manager
            .diff_base(&SpaceSelector::id(&restored.id))
            .await
            .unwrap()
            .commit,
        head
    );
}

#[tokio::test]
async fn reopening_a_prs_workspace_gives_it_to_its_opener() {
    let (temp, manager) = manager().await;
    let origin = repository(temp.path(), "origin");
    git(
        temp.path(),
        &["clone", "-q", origin.to_str().unwrap(), "clone"],
    );
    std::fs::write(origin.join("fork"), "fork\n").unwrap();
    commit(&origin, "fork");
    let head = git(&origin, &["rev-parse", "HEAD"]).trim().to_owned();
    git(&origin, &["update-ref", "refs/pull/4/head", &head]);
    let repo = manager
        .register_repository(
            temp.path().join("clone").to_str().unwrap().into(),
            None,
            None,
        )
        .await
        .unwrap();
    let start = |name: &str| {
        manager.start_chat(
            repo.id.clone().into(),
            name.into(),
            "claude".into(),
            std::process::id(),
            None,
            false,
        )
    };
    let (first, second) = (
        start("first").await.unwrap(),
        start("second").await.unwrap(),
    );
    let open = |chat: Option<SpaceId>, confined: bool| {
        manager.open_pull_head(
            repo.id.as_str().into(),
            "https://forge.example/team/repo/pulls/4".into(),
            crate::protocol::PullHead {
                number: 4,
                reference: "refs/pull/4/head".into(),
                commit: head.clone(),
                base: None,
            },
            chat,
            Default::default(),
            confined,
        )
    };
    let opened = open(Some(first.id.clone()), true).await.unwrap().workspace;
    assert_eq!(
        opened.parent.map(|parent| parent.id),
        Some(first.id.clone())
    );
    // Another chat's own agent cannot take it from the first.
    open(Some(second.id.clone()), true).await.unwrap_err();
    let kept = manager.workspace_by_id(&opened.id).await.unwrap();
    assert_eq!(kept.parent.map(|parent| parent.id), Some(first.id.clone()));
    let reopened = open(Some(second.id.clone()), false)
        .await
        .unwrap()
        .workspace;
    assert_eq!(
        reopened.parent.map(|parent| parent.id),
        Some(second.id.clone())
    );
    let by_user = open(None, false).await.unwrap().workspace;
    assert_eq!(by_user.parent, None);
}

#[tokio::test]
async fn a_workspace_at_a_commit_takes_no_part_in_a_stack() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let branch = manager
        .create_workspace(&repo.id, "feature".into(), None, None, None)
        .await
        .unwrap();
    let detached = create(&manager, &repo.id, None, CommitStart::Revision(None))
        .await
        .unwrap();
    let as_base = manager
        .set_base_workspace(
            &SpaceSelector::id(&branch.id),
            Some(detached.name.to_string()),
        )
        .await;
    assert!(as_base.is_err());
    let stacked = manager
        .set_base_workspace(
            &SpaceSelector::id(&detached.id),
            Some(branch.name.to_string()),
        )
        .await;
    assert!(stacked.is_err());
}

#[tokio::test]
async fn a_parent_of_another_repository_is_refused_before_any_fetch() {
    let (temp, manager) = manager().await;
    let own = repository(temp.path(), "own");
    let origin = repository(temp.path(), "origin");
    git(
        temp.path(),
        &["clone", "-q", origin.to_str().unwrap(), "other"],
    );
    let other = temp.path().join("other");
    // A tag only origin has would make resolving it fetch the other clone.
    git(&origin, &["tag", "remote-only"]);
    let own = manager
        .register_repository(own.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let other_repo = manager
        .register_repository(other.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&own.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let refused = create(
        &manager,
        &other_repo.id,
        Some(&parent.id),
        CommitStart::Revision(Some("remote-only".into())),
    )
    .await;
    assert!(refused.is_err());
    assert!(git(&other, &["tag", "--list", "remote-only"]).is_empty());
}

#[tokio::test]
async fn a_chat_opens_no_workspace_at_a_commit_in_another_repository() {
    let (temp, manager) = manager().await;
    let mut repos = Vec::new();
    for name in ["repo", "other"] {
        let checkout = repository(temp.path(), name);
        repos.push(
            manager
                .register_repository(checkout.to_str().unwrap().into(), None, None)
                .await
                .unwrap(),
        );
    }
    let chat = manager
        .start_chat(
            repos[0].id.clone().into(),
            "planning".into(),
            "claude".into(),
            std::process::id(),
            None,
            false,
        )
        .await
        .unwrap();
    let other = &repos[1];
    manager
        .create_commit_workspace(
            other.id.as_str().into(),
            Some(chat.id.clone()),
            CommitStart::Revision(None),
            Default::default(),
        )
        .await
        .unwrap_err();
    manager
        .open_pull_head(
            other.id.as_str().into(),
            "https://forge.example/team/other/pulls/3".into(),
            crate::protocol::PullHead {
                number: 3,
                reference: "refs/pull/3/head".into(),
                commit: "0000000".into(),
                base: None,
            },
            Some(chat.id.clone()),
            Default::default(),
            true,
        )
        .await
        .unwrap_err();
    // Refused before anything was created.
    assert!(manager.list_workspaces().await.unwrap().is_empty());
}
