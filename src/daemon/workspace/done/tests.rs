use super::*;
use crate::{
    forge::pr::Action,
    test_support::{commit, git, manager, repository},
};
use std::{fs, sync::Arc};

#[tokio::test]
async fn completion_without_a_hook_allows_concurrent_resource_hooks() {
    let (_root, manager, workspace) = fixture().await;
    let resources = manager
        .resource_guard(&workspace.id, GuardMode::Exclusive)
        .await
        .unwrap();
    manager
        .mark_done(&workspace.id, Some(false), Requester::Unscoped)
        .await
        .unwrap();
    manager
        .set_repository_config(
            &workspace.repository_id,
            Some("post_done_cmd = '/usr/bin/true'\n".into()),
        )
        .await
        .unwrap();
    let error = manager
        .mark_done(&workspace.id, Some(true), Requester::Unscoped)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("resource operation is in progress")
    );
    assert!(
        !manager
            .completion(&workspace.id)
            .await
            .unwrap()
            .unwrap()
            .cleanup
    );
    drop(resources);
    assert!(
        manager
            .mark_done(&workspace.id, Some(true), Requester::Unscoped)
            .await
            .unwrap()
            .cleanup
    );
}

#[tokio::test]
async fn completion_persists_and_explicit_choices_override_the_repository_default() {
    let (_root, manager, workspace) = fixture().await;
    let repo = manager.repository(&workspace.repository_id).await.unwrap();
    assert!(
        manager
            .mark_done(&workspace.id, None, Requester::Unscoped)
            .await
            .unwrap()
            .cleanup
    );
    manager
        .set_repository_config(&repo.id, Some("[done]\ncleanup=false\n".into()))
        .await
        .unwrap();
    assert!(
        !manager
            .mark_done(&workspace.id, None, Requester::Unscoped)
            .await
            .unwrap()
            .cleanup
    );
    assert!(
        manager
            .mark_done(&workspace.id, Some(true), Requester::Unscoped)
            .await
            .unwrap()
            .cleanup
    );
    manager
        .set_repository_config(&repo.id, Some("[done]\ncleanup=true\n".into()))
        .await
        .unwrap();
    let kept = manager
        .mark_done(&workspace.id, Some(false), Requester::Unscoped)
        .await
        .unwrap();
    assert!(!kept.cleanup);
    let reopened = Manager::open(manager.paths.clone()).await.unwrap();
    let completion = reopened
        .inspect_workspace(&workspace.id)
        .await
        .unwrap()
        .completion
        .unwrap();
    assert!(!completion.cleanup);
    assert_eq!(completion.head, kept.head);
    assert!(
        reopened
            .notifications(true, 100)
            .await
            .unwrap()
            .iter()
            .any(|event| event.kind == NotificationKind::WorkspaceDone)
    );
    reopened.store.shutdown().await;
}

async fn fixture() -> (tempfile::TempDir, Arc<Manager>, Workspace) {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "finished".into(), None, None, None)
        .await
        .unwrap();
    (root, manager, workspace)
}

#[tokio::test]
async fn keep_blocks_idle_and_pr_cleanup_until_explicitly_changed() {
    let (_root, manager, workspace) = fixture().await;
    manager
        .set_pr(&workspace.id, Action::Acknowledge)
        .await
        .unwrap();
    manager
        .mark_done(&workspace.id, Some(false), Requester::Unscoped)
        .await
        .unwrap();
    let reopened = Manager::open(manager.paths.clone()).await.unwrap();
    assert!(
        reopened
            .cleanup_snapshot(&workspace.id)
            .await
            .unwrap()
            .is_none()
    );
    reopened.sweep_completed().await.unwrap();
    reopened.sweep_prs().await.unwrap();
    assert!(workspace.path.exists());
    reopened
        .mark_done(&workspace.id, Some(true), Requester::Unscoped)
        .await
        .unwrap();
    reopened.sweep_prs().await.unwrap();
    assert!(!workspace.path.exists());
    assert!(reopened.completion(&workspace.id).await.unwrap().is_none());
    reopened.store.shutdown().await;
}

#[tokio::test]
async fn completion_retains_dirty_unpushed_and_newer_work() {
    let (_root, manager, workspace) = fixture().await;
    let file = workspace.path.join("new-work");
    fs::write(&file, "keep").unwrap();
    manager
        .mark_done(&workspace.id, None, Requester::Unscoped)
        .await
        .unwrap();
    manager.sweep_completed().await.unwrap();
    assert_error(&manager, &workspace, "uncommitted").await;
    commit(&workspace.path, "new-work");
    manager
        .mark_done(&workspace.id, None, Requester::Unscoped)
        .await
        .unwrap();
    manager.sweep_completed().await.unwrap();
    assert_error(&manager, &workspace, "neither pushed").await;
    // Simulate a fetched remote-tracking branch preserving the completed HEAD.
    git(
        &workspace.path,
        &["update-ref", "refs/remotes/origin/finished", "HEAD"],
    );
    // A later clean, fully preserved commit must not inherit the old cleanup request.
    fs::write(&file, "newer").unwrap();
    commit(&workspace.path, "new-work");
    git(
        &workspace.path,
        &["update-ref", "refs/remotes/origin/finished", "HEAD"],
    );
    let reopened = Manager::open(manager.paths.clone()).await.unwrap();
    reopened.sweep_completed().await.unwrap();
    assert_error(&reopened, &workspace, "HEAD changed").await;
    assert!(
        reopened
            .cleanup_snapshot(&workspace.id)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), "newer");
    reopened
        .mark_done(&workspace.id, Some(true), Requester::Unscoped)
        .await
        .unwrap();
    reopened.sweep_completed().await.unwrap();
    assert!(!workspace.path.exists());
    // A nonredundant branch is retained even when its commits are pushed.
    let repo = reopened.repository(&workspace.repository_id).await.unwrap();
    git(&repo.path, &["show-ref", "--verify", "refs/heads/finished"]);
    reopened.store.shutdown().await;
}

async fn assert_error(manager: &Manager, workspace: &Workspace, expected: &str) {
    let completion = manager.completion(&workspace.id).await.unwrap().unwrap();
    assert!(
        completion.error.as_deref().unwrap().contains(expected),
        "{completion:?}"
    );
    assert!(workspace.path.exists());
}

#[tokio::test]
async fn completion_rechecks_work_after_removal_hooks() {
    use std::os::unix::fs::PermissionsExt;
    let (root, manager, workspace) = fixture().await;
    let hook = root.path().join("modify-work");
    fs::write(&hook, "#!/bin/sh\nprintf 'preserve me' > new-work\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    manager
        .set_repository_config(
            &workspace.repository_id,
            Some(format!("pre_remove_cmd = '{}'\n", hook.display())),
        )
        .await
        .unwrap();
    manager
        .mark_done(&workspace.id, None, Requester::Unscoped)
        .await
        .unwrap();
    manager.sweep_completed().await.unwrap();
    assert_error(&manager, &workspace, "uncommitted").await;
    assert_eq!(
        fs::read_to_string(workspace.path.join("new-work")).unwrap(),
        "preserve me"
    );
}

#[tokio::test]
async fn undone_withdraws_completion_without_blocking_cleanup() {
    let (_root, manager, workspace) = fixture().await;
    manager
        .mark_done(&workspace.id, Some(true), Requester::Unscoped)
        .await
        .unwrap();
    let withdrawn = manager.undo_done(&workspace.id).await.unwrap().unwrap();
    assert!(withdrawn.cleanup);
    assert!(manager.undo_done(&workspace.id).await.unwrap().is_none());
    assert!(manager.completion(&workspace.id).await.unwrap().is_none());
    // Without completion, links or holds the workspace is an idle candidate again.
    assert!(
        manager
            .cleanup_snapshot(&workspace.id)
            .await
            .unwrap()
            .is_some()
    );
    // A hold, not the withdrawn completion, keeps a merged workspace.
    manager
        .acquire_hold(&workspace.id, "more-work".into(), None)
        .await
        .unwrap();
    manager
        .set_pr(&workspace.id, Action::Acknowledge)
        .await
        .unwrap();
    manager.sweep_prs().await.unwrap();
    assert!(workspace.path.exists());
    manager
        .release_hold(&workspace.id, "more-work".into())
        .await
        .unwrap();
    manager.sweep_prs().await.unwrap();
    assert!(!workspace.path.exists());
}

#[tokio::test]
async fn explicit_done_releases_only_the_hold_migrated_from_continuation() {
    let (_root, manager, workspace) = fixture().await;
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            // A user hold with the same name and reason is not the migrated one.
            db.execute(
                "INSERT INTO workspace_holds(workspace_id,name,reason,created_at,from_continuation)
                 VALUES (?1,'continue','Converted from an earlier Shoal version',1,1),
                        (?1,'thread','Converted from an earlier Shoal version',1,0)",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let names = || async {
        let holds = manager.workspace(&workspace.id).await.unwrap().holds;
        holds.into_iter().map(|hold| hold.name).collect::<Vec<_>>()
    };
    assert_eq!(names().await, ["continue", "thread"]);
    manager
        .mark_done(&workspace.id, Some(false), Requester::Unscoped)
        .await
        .unwrap();
    assert_eq!(names().await, ["thread"]);
}

#[tokio::test]
async fn undone_keeps_an_unreadable_completion() {
    let (_root, manager, workspace) = fixture().await;
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO workspace_completion(workspace_id,record,cause) VALUES (?1,'{','manual')",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(manager.undo_done(&workspace.id).await.is_err());
    assert!(manager.completion(&workspace.id).await.is_err());
}

#[tokio::test]
async fn issue_and_watch_completion_wait_for_explicit_done() {
    let (_root, manager, workspace) = fixture().await;
    let id = workspace.id.clone();
    let record = serde_json::json!({
        "url": "https://github.com/team/repo/pull/1",
        "head": null,
        "error": null,
    })
    .to_string();
    manager
        .store
        .run(move |db| {
            // Without an origin, any issue or PR lookup would record an error.
            db.execute(
                "INSERT INTO pr_cleanup(workspace_id,record) VALUES (?1,?2)",
                rusqlite::params![id, record],
            )?;
            db.execute(
                "INSERT INTO workspace_issue(workspace_id,url) VALUES (?1,?2)",
                rusqlite::params![id, "https://github.com/team/repo/issues/1"],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    manager.sweep_prs().await.unwrap();
    let inspection = manager.inspect_workspace(&workspace.id).await.unwrap();
    assert!(inspection.completion.is_none());
    assert!(inspection.issue.unwrap().error.is_none());
    assert!(inspection.pr_cleanup.unwrap().error.is_none());
    // Explicit completion applies the watch's merge checks.
    manager
        .mark_done(&workspace.id, Some(true), Requester::Unscoped)
        .await
        .unwrap();
    manager.sweep_prs().await.unwrap();
    let inspection = manager.inspect_workspace(&workspace.id).await.unwrap();
    assert!(
        inspection
            .pr_cleanup
            .unwrap()
            .error
            .unwrap()
            .contains("origin")
    );
    assert!(workspace.path.exists());
}

#[tokio::test]
async fn holds_retain_completion_without_turning_it_into_an_error() {
    let (_root, manager, workspace) = fixture().await;
    manager
        .acquire_hold(&workspace.id, "app".into(), None)
        .await
        .unwrap();
    let completed = manager
        .mark_done(&workspace.id, None, Requester::Unscoped)
        .await
        .unwrap();
    manager.sweep_completed().await.unwrap();
    let completion = manager.completion(&workspace.id).await.unwrap().unwrap();
    assert_eq!(completion.head, completed.head);
    assert!(completion.cleanup);
    assert!(completion.error.is_none());
    assert!(workspace.path.exists());
    manager
        .release_hold(&workspace.id, "app".into())
        .await
        .unwrap();
    manager.sweep_completed().await.unwrap();
    assert!(!workspace.path.exists());
}

fn agent(reason: &str) -> Requester {
    Requester::Agent {
        reason: Some(reason.into()),
    }
}

#[tokio::test]
async fn agent_keep_waits_for_the_user_and_denial_requests_cleanup() {
    let (_root, manager, workspace) = fixture().await;
    let pending = manager
        .mark_done(&workspace.id, Some(false), agent("review the UI"))
        .await
        .unwrap();
    assert!(!pending.cleanup);
    let id = pending.keep_request.unwrap();
    // Repeating the request keeps the one the user is deciding on.
    let repeated = manager
        .mark_done(&workspace.id, Some(false), agent("other words"))
        .await
        .unwrap();
    assert_eq!(repeated.keep_request.as_ref(), Some(&id));
    let requests = manager
        .approval_requests(Some(&workspace.id))
        .await
        .unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].reason, "review the UI");
    manager.sweep_completed().await.unwrap();
    assert!(workspace.path.exists());

    manager.decide_approval(id, false).await.unwrap();
    let completion = manager.completion(&workspace.id).await.unwrap().unwrap();
    assert!(completion.cleanup);
    assert!(completion.keep_request.is_none());
    let error = manager
        .mark_done(&workspace.id, Some(false), agent("review the UI"))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("denied"), "{error:#}");
    manager.sweep_completed().await.unwrap();
    assert!(!workspace.path.exists());
}

#[tokio::test]
async fn approved_keep_covers_its_head_and_other_choices_cancel_the_request() {
    let (_root, manager, workspace) = fixture().await;
    let id = manager
        .mark_done(&workspace.id, Some(false), agent("review"))
        .await
        .unwrap()
        .keep_request
        .unwrap();
    manager.decide_approval(id, true).await.unwrap();
    let messages = manager.agent_messages(&workspace.id).await.unwrap();
    assert_eq!(
        messages[0].message,
        format!("Approval {id} for keep was approved: Shoal keeps this workspace.")
    );
    let completion = manager.completion(&workspace.id).await.unwrap().unwrap();
    assert!(!completion.cleanup);
    assert!(completion.keep_request.is_none());
    let again = manager
        .mark_done(&workspace.id, Some(false), agent("review"))
        .await
        .unwrap();
    assert!(!again.cleanup);
    assert!(again.keep_request.is_none());

    fs::write(workspace.path.join("new-work"), "more").unwrap();
    commit(&workspace.path, "new-work");
    let next = manager
        .mark_done(&workspace.id, Some(false), agent("review again"))
        .await
        .unwrap()
        .keep_request
        .unwrap();
    assert_ne!(next, id);
    manager.undo_done(&workspace.id).await.unwrap();
    assert!(
        manager
            .approval_requests(Some(&workspace.id))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(manager.decide_approval(next, true).await.is_err());
}

#[tokio::test]
async fn agent_keep_needs_approval_only_against_configured_cleanup() {
    let (_root, manager, workspace) = fixture().await;
    let unexplained = Requester::Agent { reason: None };
    let error = manager
        .mark_done(&workspace.id, Some(false), unexplained.clone())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("--reason"), "{error:#}");
    assert!(manager.completion(&workspace.id).await.unwrap().is_none());
    let user = manager
        .mark_done(&workspace.id, Some(false), Requester::Unscoped)
        .await
        .unwrap();
    assert!(user.keep_request.is_none());

    manager
        .set_repository_config(
            &workspace.repository_id,
            Some("[done]\ncleanup = false\n".into()),
        )
        .await
        .unwrap();
    for cleanup in [None, Some(false)] {
        let completion = manager
            .mark_done(&workspace.id, cleanup, unexplained.clone())
            .await
            .unwrap();
        assert!(!completion.cleanup);
        assert!(completion.keep_request.is_none());
    }
}
