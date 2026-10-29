use super::*;
use crate::{
    daemon::links::pr::Action,
    model::CleanupReason,
    test_support::{commit, git, manager, repository},
};
use std::{fs, sync::Arc};

#[tokio::test]
async fn completion_without_a_hook_allows_concurrent_resource_hooks() {
    let (_root, manager, workspace) = fixture().await;
    manager
        .set_repository_config(
            &workspace.repository_id,
            Some("[done]\ncleanup = false\n".into()),
        )
        .await
        .unwrap();
    let resources = manager
        .gates
        .resource_guard(&workspace.id, GuardMode::Exclusive)
        .unwrap();
    manager
        .mark_done(&workspace.id, false, false, vec![])
        .await
        .unwrap();
    manager
        .set_repository_config(
            &workspace.repository_id,
            Some("post_done_cmd = '/usr/bin/true'\n[done]\ncleanup = false\n".into()),
        )
        .await
        .unwrap();
    let error = manager
        .mark_done(&workspace.id, true, false, vec![])
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
            .mark_done(&workspace.id, true, false, vec![])
            .await
            .unwrap()
            .cleanup
    );
}

#[tokio::test]
async fn completion_persists_and_cleanup_overrides_the_repository_default() {
    let (_root, manager, workspace) = fixture().await;
    let repo = manager.repository(&workspace.repository_id).await.unwrap();
    assert!(
        manager
            .mark_done(&workspace.id, false, false, vec![])
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
            .mark_done(&workspace.id, false, false, vec![])
            .await
            .unwrap()
            .cleanup
    );
    assert!(
        manager
            .mark_done(&workspace.id, true, false, vec![])
            .await
            .unwrap()
            .cleanup
    );
    let kept = manager
        .mark_done(&workspace.id, false, false, vec![])
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

#[tokio::test]
async fn repeating_an_unchanged_completion_is_refused() {
    let (_root, manager, workspace) = fixture().await;
    manager
        .set_repository_config(
            &workspace.repository_id,
            Some("[done]\ncleanup = false\n".into()),
        )
        .await
        .unwrap();
    manager
        .mark_done(&workspace.id, false, false, vec![])
        .await
        .unwrap();
    let error = manager
        .mark_done(&workspace.id, false, false, vec![])
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("already marked done"),
        "{error:#}"
    );
    // Repeating it closes a question asked since.
    manager
        .ask_question(
            &workspace.name,
            crate::daemon::questions::Ask {
                question: "Anything more?".into(),
                recommendation: "No".into(),
                reason: None,
                choices: vec![],
            },
        )
        .await
        .unwrap();
    manager
        .mark_done(&workspace.id, false, false, vec![])
        .await
        .unwrap();
    assert!(!manager.has_open_questions(&workspace.id).await.unwrap());
    // Another cleanup choice or HEAD replaces the recorded completion.
    assert!(
        manager
            .mark_done(&workspace.id, true, false, vec![])
            .await
            .unwrap()
            .cleanup
    );
    fs::write(workspace.path.join("more-work"), "more").unwrap();
    commit(&workspace.path, "more-work");
    let newer = manager
        .mark_done(&workspace.id, true, false, vec![])
        .await
        .unwrap();
    assert_eq!(
        manager
            .completion(&workspace.id)
            .await
            .unwrap()
            .unwrap()
            .head,
        newer.head
    );
}

#[tokio::test]
async fn repeating_a_completion_reruns_a_configured_hook() {
    use std::os::unix::fs::PermissionsExt;
    let (root, manager, workspace) = fixture().await;
    let runs = root.path().join("runs");
    let hook = root.path().join("post-done");
    fs::write(
        &hook,
        format!("#!/bin/sh\nprintf x >> '{}'\nexit 1\n", runs.display()),
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    manager
        .set_repository_config(
            &workspace.repository_id,
            Some(format!(
                "post_done_cmd = '{}'\n[done]\ncleanup = false\n",
                hook.display()
            )),
        )
        .await
        .unwrap();
    manager
        .mark_done(&workspace.id, false, false, vec![])
        .await
        .unwrap();
    // After the hook failed, the same completion runs it again.
    manager
        .mark_done(&workspace.id, false, false, vec![])
        .await
        .unwrap();
    assert_eq!(fs::read_to_string(&runs).unwrap(), "xx");
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
async fn completion_without_cleanup_blocks_idle_and_pr_cleanup_until_requested() {
    let (_root, manager, workspace) = fixture().await;
    manager
        .set_repository_config(
            &workspace.repository_id,
            Some("[done]\ncleanup = false\n".into()),
        )
        .await
        .unwrap();
    manager
        .set_pr(&workspace.id, Action::Acknowledge)
        .await
        .unwrap();
    manager
        .mark_done(&workspace.id, false, false, vec![])
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
        .mark_done(&workspace.id, true, false, vec![])
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
        .mark_done(&workspace.id, false, false, vec![])
        .await
        .unwrap();
    manager.sweep_completed().await.unwrap();
    assert_error(&manager, &workspace, "uncommitted").await;
    commit(&workspace.path, "new-work");
    manager
        .mark_done(&workspace.id, false, false, vec![])
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
        .mark_done(&workspace.id, true, false, vec![])
        .await
        .unwrap();
    reopened.sweep_completed().await.unwrap();
    assert!(!workspace.path.exists());
    // A branch whose commits a remote has goes with the workspace.
    let repo = reopened.repository(&workspace.repository_id).await.unwrap();
    assert_eq!(git(&repo.path, &["branch", "--list", "finished"]), "");
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
        .mark_done(&workspace.id, false, false, vec![])
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
async fn a_removal_holds_only_its_own_workspaces_gate() {
    use std::os::unix::fs::PermissionsExt;
    let (root, manager, workspace) = fixture().await;
    let other = manager
        .create_workspace(&workspace.repository_id, "other".into(), None, None, None)
        .await
        .unwrap();
    let (started, release) = (root.path().join("started"), root.path().join("release"));
    let hook = root.path().join("wait-for-release");
    fs::write(
        &hook,
        format!(
            "#!/bin/sh\ntouch '{}'\nwhile [ ! -e '{}' ]; do sleep 0.01; done\n",
            started.display(),
            release.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    manager
        .set_repository_config(
            &workspace.repository_id,
            Some(format!("pre_remove_cmd = '{}'\n", hook.display())),
        )
        .await
        .unwrap();
    manager
        .mark_done(&workspace.id, true, false, vec![])
        .await
        .unwrap();
    let sweep = tokio::spawn({
        let manager = manager.clone();
        async move { manager.sweep_completed().await }
    });
    bounded(async {
        while !started.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(manager.gates.try_lock_prs([&workspace.id]).is_none());
    // Another workspace's commands do not wait for the removal.
    bounded(manager.mark_done(&other.id, false, false, vec![]))
        .await
        .unwrap();
    bounded(manager.undo_done(&other.id)).await.unwrap();
    assert!(!sweep.is_finished());
    fs::write(&release, "").unwrap();
    bounded(sweep).await.unwrap().unwrap();
    assert!(!workspace.path.exists());
    assert!(other.path.exists());
}

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(std::time::Duration::from_secs(60), future)
        .await
        .expect("operation timed out")
}

#[tokio::test]
async fn undone_withdraws_completion_without_blocking_cleanup() {
    let (_root, manager, workspace) = fixture().await;
    manager
        .mark_done(&workspace.id, true, false, vec![])
        .await
        .unwrap();
    let withdrawn = manager.undo_done(&workspace.id).await.unwrap();
    assert!(withdrawn.cleanup);
    let error = manager.undo_done(&workspace.id).await.unwrap_err();
    assert!(error.to_string().contains("not marked done"), "{error:#}");
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
        .acquire_hold(&workspace.id, "review".into(), None)
        .await
        .unwrap();
    manager
        .set_pr(&workspace.id, Action::Acknowledge)
        .await
        .unwrap();
    manager.sweep_prs().await.unwrap();
    assert!(workspace.path.exists());
    manager
        .release_hold(&workspace.id, "review".into())
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
        .mark_done(&workspace.id, false, false, vec![])
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
        .mark_done(&workspace.id, true, false, vec![])
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
        .mark_done(&workspace.id, false, false, vec![])
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

#[tokio::test]
async fn questions_asked_after_done_retain_completion_and_pr_cleanup() {
    let (_root, manager, workspace) = fixture().await;
    let ask = |question: &str| crate::daemon::questions::Ask {
        question: question.into(),
        recommendation: "No".into(),
        reason: None,
        choices: vec![],
    };
    // Done closes the questions asked before it.
    manager
        .ask_question(&workspace.name, ask("Anything else?"))
        .await
        .unwrap();
    manager
        .mark_done(&workspace.id, false, false, vec![])
        .await
        .unwrap();
    let question = manager
        .ask_question(&workspace.name, ask("Anything more?"))
        .await
        .unwrap();
    manager.sweep_completed().await.unwrap();
    assert!(workspace.path.exists());
    manager
        .set_pr(&workspace.id, Action::Acknowledge)
        .await
        .unwrap();
    manager.sweep_prs().await.unwrap();
    assert!(workspace.path.exists());
    let completion = manager.completion(&workspace.id).await.unwrap().unwrap();
    assert!(completion.error.is_none());
    // A question asked after a sweep checked for one still stops its removal.
    for error in [
        manager
            .remove_completed(&workspace.id, &completion, EventCause::Completion)
            .await
            .unwrap_err(),
        manager
            .remove_merged(&workspace.id, &completion.head)
            .await
            .unwrap_err(),
    ] {
        assert!(error.to_string().contains("question"), "{error:#}");
    }
    assert!(workspace.path.exists());
    assert!(manager.has_open_questions(&workspace.id).await.unwrap());
    manager
        .answer_question(question.id, "No".into(), None)
        .await
        .unwrap();
    manager.sweep_prs().await.unwrap();
    assert!(!workspace.path.exists());
}

#[tokio::test]
async fn completion_cleans_up_commits_the_start_branch_took_unchanged() {
    let (_root, manager, base, workspace) = stacked_fixture().await;
    fs::write(base.path.join("base-work"), "base").unwrap();
    commit(&base.path, "base-work");
    cherry_pick(&base.path, &workspace.branch);
    // The idle rule counts the commits as kept as well.
    assert!(
        manager
            .cleanup_snapshot(&workspace.id)
            .await
            .unwrap()
            .is_some()
    );
    manager
        .mark_done(&workspace.id, false, false, vec![])
        .await
        .unwrap();
    manager.sweep_completed().await.unwrap();
    assert!(!workspace.path.exists());
    // Neither a remote nor the default branch has its commits.
    let repo = manager.repository(&workspace.repository_id).await.unwrap();
    assert_ne!(git(&repo.path, &["branch", "--list", "part"]), "");
}

#[tokio::test]
async fn completion_retains_commits_the_start_branch_took_adapted() {
    let (_root, manager, base, workspace) = stacked_fixture().await;
    fs::write(base.path.join("part-work"), "adapted").unwrap();
    commit(&base.path, "part-work");
    assert_eq!(
        manager.cleanup_candidacy(&workspace.id).await.unwrap(),
        Err(CleanupReason::Unpushed { commits: 1 })
    );
    manager
        .mark_done(&workspace.id, false, false, vec![])
        .await
        .unwrap();
    manager.sweep_completed().await.unwrap();
    assert_error(&manager, &workspace, "neither pushed").await;
    // Marking the same HEAD integrated updates the completion.
    let completion = manager
        .mark_done(&workspace.id, false, true, vec![])
        .await
        .unwrap();
    assert!(completion.integrated && completion.error.is_none());
    assert_eq!(
        manager.cleanup_candidacy(&workspace.id).await.unwrap(),
        Err(CleanupReason::Completed { integrated: true })
    );
    manager.sweep_completed().await.unwrap();
    assert!(!workspace.path.exists());
    let repo = manager.repository(&workspace.repository_id).await.unwrap();
    assert_ne!(git(&repo.path, &["branch", "--list", "part"]), "");
}

#[tokio::test]
async fn an_integrated_completion_retains_dirty_and_newer_work() {
    let (_root, manager, workspace) = fixture().await;
    fs::write(workspace.path.join("new-work"), "keep").unwrap();
    commit(&workspace.path, "new-work");
    fs::write(workspace.path.join("unsaved"), "keep").unwrap();
    manager
        .mark_done(&workspace.id, false, true, vec![])
        .await
        .unwrap();
    manager.sweep_completed().await.unwrap();
    assert_error(&manager, &workspace, "uncommitted").await;
    // A commit after the mark lapses it like any completion.
    commit(&workspace.path, "unsaved");
    manager.sweep_completed().await.unwrap();
    assert_error(&manager, &workspace, "HEAD changed").await;
}

#[tokio::test]
async fn plain_done_returns_an_integrated_completion_to_normal_retention() {
    let (_root, manager, workspace) = fixture().await;
    fs::write(workspace.path.join("new-work"), "keep").unwrap();
    commit(&workspace.path, "new-work");
    manager
        .acquire_hold(&workspace.id, "app".into(), None)
        .await
        .unwrap();
    manager
        .mark_done(&workspace.id, false, true, vec![])
        .await
        .unwrap();
    let completion = manager
        .mark_done(&workspace.id, false, false, vec![])
        .await
        .unwrap();
    assert!(!completion.integrated);
    manager
        .release_hold(&workspace.id, "app".into())
        .await
        .unwrap();
    manager.sweep_completed().await.unwrap();
    assert_error(&manager, &workspace, "neither pushed").await;
}

/// A workspace `part` stacked on the workspace `finished`, with one commit
/// of its own.
async fn stacked_fixture() -> (tempfile::TempDir, Arc<Manager>, Workspace, Workspace) {
    let (root, manager, base) = fixture().await;
    let workspace = manager
        .create_workspace(
            &base.repository_id,
            "part".into(),
            Some(base.branch.clone()),
            None,
            None,
        )
        .await
        .unwrap();
    fs::write(workspace.path.join("part-work"), "part").unwrap();
    commit(&workspace.path, "part-work");
    (root, manager, base, workspace)
}

fn cherry_pick(repo: &std::path::Path, revision: &str) {
    git(
        repo,
        &[
            "-c",
            "user.name=Shoal Test",
            "-c",
            "user.email=shoal@example.invalid",
            "cherry-pick",
            revision,
        ],
    );
}

#[test]
fn a_notice_names_the_first_results_and_counts_the_rest() {
    let results: Vec<PathBuf> = ["report.md", "repro.sh", "trace.log", "a.png", "b.png"]
        .iter()
        .map(|name| PathBuf::from("/attachments/results/id/key/0").join(name))
        .collect();
    assert_eq!(
        results_preview(&results),
        "report.md, repro.sh, trace.log and 2 more"
    );
    assert_eq!(results_preview(&results[..2]), "report.md, repro.sh");
}
