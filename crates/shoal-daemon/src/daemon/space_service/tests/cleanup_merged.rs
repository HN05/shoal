use crate::daemon::cleanup::merged::*;
use crate::daemon::manager::Manager;
use crate::execution::recovery::RecordExt;
use crate::model::EventCause;
use crate::paths::StateFiles;
use crate::time::UnixTime;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use std::path::PathBuf;
use std::time::Duration;
use std::{fs, sync::Arc};
use tokio::time::Instant;

use crate::daemon::test_support::{commit, git, manager, repository};
use crate::{
    execution::recovery,
    model::{CleanupReason, Workspace},
};
use shoal_protocol::space_names::SpaceSelector;

const HOUR: Duration = Duration::from_secs(3600);

/// Local repository config that turns merged cleanup on.
const ENABLED: &str = "[cleanup.merged]\nenabled = true\n";

struct Fixture {
    _root: tempfile::TempDir,
    manager: Arc<Manager>,
    checkout: PathBuf,
    workspace: Workspace,
}

impl Fixture {
    /// A workspace with two commits and a linked issue, which keeps it from
    /// idle cleanup, in a repository with merged cleanup on.
    async fn new() -> Self {
        let fixture = Self::with_config(ENABLED).await;
        fixture.link_issue().await;
        fixture
    }

    /// A workspace with two commits in a repository with local `config`.
    async fn with_config(config: &str) -> Self {
        let (root, manager) = manager().await;
        let checkout = repository(root.path(), "repo");
        let repo = manager
            .repositories
            .register(checkout.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        (manager.write_local_config(&repo.id, Some(config.into())))
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "topic".into(), None, None, None)
            .await
            .unwrap();
        for file in ["one", "two"] {
            fs::write(workspace.path.join(file), format!("{file}\n")).unwrap();
            commit(&workspace.path, file);
        }
        Self {
            _root: root,
            manager,
            checkout,
            workspace,
        }
    }

    async fn link_issue(&self) {
        let id = self.workspace.id.clone();
        self.manager
            .store
            .run(move |db| {
                db.execute(
                    "INSERT INTO space_issue(space_id,url) VALUES (?1,'http://127.0.0.1:1/team/repo/issues/1')",
                    [id],
                )?;
                Ok(())
            })
            .await
            .unwrap();
    }

    /// Squash the branch onto the default branch, as a forge merge would.
    fn squash_merge(&self) {
        git(&self.checkout, &["merge", "-q", "--squash", "topic"]);
        git(&self.checkout, &["commit", "-q", "-m", "squash"]);
    }

    async fn unlink_issue(&self) {
        let id = self.workspace.id.clone();
        self.manager
            .store
            .run(move |db| {
                db.execute("DELETE FROM space_issue WHERE space_id=?1", [id])?;
                Ok(())
            })
            .await
            .unwrap();
    }

    /// Let recorded activity no longer keep the workspace, so only its files do.
    async fn age_out_activity(&self) {
        let id = self.workspace.id.clone();
        self.manager
            .store
            .run(move |db| {
                db.execute("UPDATE workspaces SET active_at=0 WHERE id=?1", [id])?;
                Ok(())
            })
            .await
            .unwrap();
    }

    /// Run a pass `hours` from now and report whether the workspace remains.
    async fn kept_after(&self, hours: u32) -> bool {
        let mut state = State::default();
        sweep_at(
            &*self.manager,
            &mut state,
            UnixTime::now() + hours * HOUR,
            &Default::default(),
        )
        .await
        .unwrap();
        self.manager
            .spaces
            .list_workspaces()
            .await
            .unwrap()
            .iter()
            .any(|workspace| workspace.id == self.workspace.id)
    }
}

#[tokio::test]
async fn merged_cleanup_is_off_by_default() {
    let f = Fixture::with_config("").await;
    f.squash_merge();
    assert!(f.kept_after(24).await);
}

#[tokio::test]
async fn a_space_below_keeps_a_merged_workspace_until_it_goes() {
    let f = Fixture::new().await;
    let child = (f.manager)
        .spaces
        .create_workspace_below(
            &*f.manager,
            f.workspace.repository().clone().into(),
            "child".into(),
            None,
            Default::default(),
            Some(f.workspace.id.clone()),
        )
        .await
        .unwrap();
    fs::write(child.path.join("three"), "three\n").unwrap();
    commit(&child.path, "three");
    f.squash_merge();
    assert!(f.kept_after(5).await);
    assert!(child.path.exists(), "the child's unmerged work stays");
    (f.manager)
        .spaces
        .remove_selected(
            &*f.manager,
            &SpaceSelector::id(&child.id),
            crate::removal::UnsavedWork::default(),
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert!(!f.kept_after(5).await);
}

#[tokio::test]
async fn a_saved_command_keeps_idle_and_unmerged_workspaces_only() {
    saved_work_keeps_idle_and_unmerged_workspaces_only(
        "saved.command.json",
        r#"{"argv":["cargo","test"]}"#,
    )
    .await;
}

#[tokio::test]
async fn a_saved_agent_keeps_idle_and_unmerged_workspaces_only() {
    saved_work_keeps_idle_and_unmerged_workspaces_only(
        "saved.recovery.json",
        r#"{"agent":"claude","mode":"worker","stage":"stopped"}"#,
    )
    .await;
}

/// Save `record` as the workspace's state file `name` and check that only a
/// requested cleanup removes the workspace, merged or not.
async fn saved_work_keeps_idle_and_unmerged_workspaces_only(name: &str, record: &str) {
    let f = Fixture::new().await;
    f.unlink_issue().await;
    let directory = f.manager.paths.space_state(&f.workspace.id);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join(name), record).unwrap();
    assert_eq!(
        f.manager
            .spaces
            .cleanup_candidacy(&*f.manager, &f.workspace.id)
            .await
            .unwrap(),
        Err(CleanupReason::Recovering)
    );
    assert!(f.kept_after(24).await, "unmerged saved work stays");
    f.squash_merge();
    assert_eq!(
        f.manager
            .spaces
            .cleanup_candidacy(&*f.manager, &f.workspace.id)
            .await
            .unwrap(),
        Err(CleanupReason::Recovering),
        "idle cleanup still keeps saved work after merging"
    );
    let report = crate::daemon::cleanup::on_request(
        &*f.manager,
        true,
        &crate::daemon::cleanup::Selection {
            spaces: crate::daemon::cleanup::Pick::All,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(report.removed, std::slice::from_ref(&f.workspace.name));
    assert!(report.kept.is_empty());
    assert!(!f.kept_after(5).await);
}

#[tokio::test]
async fn merged_cleanup_waits_for_grace_and_keeps_saved_agents_for_restore() {
    let f = Fixture::new().await;
    f.unlink_issue().await;
    f.squash_merge();
    f.manager
        .write_local_config(
            f.workspace.repository(),
            Some(format!("{ENABLED}[cleanup]\ngrace_minutes = 600\n")),
        )
        .await
        .unwrap();
    let path = recovery::record_path(
        &f.manager.paths,
        &f.workspace.id,
        &ExecutionId::from("saved"),
    );
    fs::create_dir_all(f.manager.paths.space_state(&f.workspace.id)).unwrap();
    let record = r#"{"agent":"claude","mode":"worker","stage":"stopped","session":"kept-session","stop_reason":"memory pressure"}"#;
    fs::write(&path, record).unwrap();
    let saved = recovery::Record::load(&path).unwrap().unwrap();
    let queued = f
        .manager
        .agents
        .queue_stopped_restore(
            &*f.manager,
            &SpaceSelector::id(&f.workspace.id),
            &ExecutionId::from("saved"),
            Default::default(),
        )
        .await
        .unwrap();

    assert!(f.kept_after(5).await);
    assert!(
        f.manager
            .spaces
            .cleanup_at(&f.workspace.id)
            .await
            .unwrap()
            .is_some()
    );
    assert!(path.exists());
    assert!(
        f.manager
            .agents
            .queue_entries(&*f.manager,)
            .await
            .unwrap()
            .iter()
            .any(|entry| entry.id == queued.id)
    );
    let id = f.workspace.id.clone();
    f.manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE cleanup_grace SET cleanup_at=0 WHERE space_id=?1",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(!f.kept_after(5).await);
    assert!(!path.exists());
    assert!(
        f.manager
            .agents
            .queue_entries(&*f.manager,)
            .await
            .unwrap()
            .is_empty()
    );
    let removed = f.manager.spaces.removed_workspaces().await.unwrap();
    let removed = removed.first().unwrap();
    assert_eq!(removed.cause, EventCause::Merged);
    assert_eq!(removed.agents.len(), 1);
    let agent = removed.agents.first().unwrap();
    assert_eq!(agent.execution, "saved");
    assert_eq!(agent.record, saved);

    let restored = f
        .manager
        .restore_workspace(&f.manager.named("topic").await)
        .await
        .unwrap();
    assert!(restored.warnings.is_empty(), "{:?}", restored.warnings);
    let workspace = restored.opened.workspace;
    assert_eq!(
        recovery::Record::load(&recovery::record_path(
            &f.manager.paths,
            &workspace.id,
            &ExecutionId::from("saved")
        ))
        .unwrap(),
        Some(saved)
    );
    assert!(
        f.manager
            .agents
            .queue_entries(&*f.manager,)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn files_never_committed_keep_a_workspace_without_commits() {
    let f = Fixture::new().await;
    git(&f.workspace.path, &["reset", "-q", "--hard", "main"]);
    fs::write(f.workspace.path.join("draft"), "draft\n").unwrap();
    assert!(f.kept_after(5).await);
    fs::remove_file(f.workspace.path.join("draft")).unwrap();
    assert!(!f.kept_after(5).await, "nothing to lose");
}

#[tokio::test]
async fn unsaved_files_of_a_merged_workspace_go_into_its_checkpoint() {
    let f = Fixture::new().await;
    f.squash_merge();
    let head = git(&f.workspace.path, &["rev-parse", "HEAD"]);
    fs::write(f.workspace.path.join("two"), "edited\n").unwrap();
    fs::write(f.workspace.path.join("scratch"), "notes\n").unwrap();
    // The archive limit caps only an agent's checkpoint.
    let repo = f.workspace.repository().clone();
    (f.manager)
        .write_local_config(
            &repo,
            Some(format!("{ENABLED}[cleanup.restore]\narchive_max_mb = 0\n")),
        )
        .await
        .unwrap();
    assert!(!f.kept_after(5).await);

    let snapshot = git(
        &f.checkout,
        &[
            "for-each-ref",
            "--format=%(objectname)",
            "refs/shoal/checkpoints/",
        ],
    );
    let snapshot = snapshot.trim();
    assert_eq!(
        git(&f.checkout, &["rev-parse", &format!("{snapshot}^")]),
        head
    );
    assert_eq!(
        git(&f.checkout, &["show", &format!("{snapshot}:two")]),
        "edited\n"
    );
    assert_eq!(
        git(&f.checkout, &["show", &format!("{snapshot}:scratch")]),
        "notes\n"
    );
    assert_eq!(
        git(&f.checkout, &["branch", "--list", "topic"]),
        "",
        "the checkpoint keeps the unsaved files, so the merged branch goes"
    );
}

#[test]
fn a_walk_sees_changes_of_nested_files_and_worktree_git_state() {
    let root = tempfile::tempdir().unwrap();
    let tree = root.path().join("tree");
    let git_dir = root.path().join("git");
    fs::create_dir_all(tree.join("a/b")).unwrap();
    fs::create_dir_all(git_dir.join("logs")).unwrap();
    // Future modification times, each newer than every change time the
    // fixture's own writes leave, so only reading that file reports it.
    let now = UnixTime::now();
    let changes = [
        (tree.join("a/b/file"), now + Duration::from_secs(1000)),
        (git_dir.join("HEAD"), now + Duration::from_secs(2000)),
        (git_dir.join("index"), now + Duration::from_secs(3000)),
        (git_dir.join("logs/HEAD"), now + Duration::from_secs(4000)),
    ];
    for (path, modified) in changes {
        fs::write(&path, "").unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(modified.system_time())
            .unwrap();
        assert_eq!(
            walk_with_git_state(&tree, Some(&git_dir)).unwrap().changed,
            modified,
            "{}",
            path.display()
        );
    }
}

#[tokio::test]
async fn a_failed_fetch_keeps_merged_workspaces() {
    let f = Fixture::new().await;
    f.squash_merge();
    let missing = f.checkout.with_file_name("missing-origin");
    git(
        &f.checkout,
        &["remote", "add", "origin", missing.to_str().unwrap()],
    );
    git(
        &f.checkout,
        &["update-ref", "refs/remotes/origin/main", "main"],
    );
    git(
        &f.checkout,
        &["branch", "--set-upstream-to=origin/main", "main"],
    );
    assert!(f.kept_after(5).await, "stale refs decide nothing");
    git(&f.checkout, &["remote", "remove", "origin"]);
    assert!(!f.kept_after(5).await);
}

#[tokio::test(start_paused = true)]
async fn a_failed_removal_is_retried_after_a_while_or_once_changed() {
    let mut state = State::default();
    assert!(!state.waits(&SpaceId::from("w"), 1));
    state.retained.insert("w".into(), (1, Instant::now()));
    assert!(state.waits(&SpaceId::from("w"), 1));
    assert!(
        !state.waits(&SpaceId::from("w"), 2),
        "HEAD or activity changed"
    );
    tokio::time::advance(RETRY_AFTER).await;
    assert!(!state.waits(&SpaceId::from("w"), 1));
}

#[tokio::test]
async fn a_recent_file_change_defers_walks_until_its_idle_period_passes() {
    let f = Fixture::new().await;
    f.squash_merge();
    f.age_out_activity().await;
    let mut state = State::default();
    let now = UnixTime::now();
    sweep_at(&*f.manager, &mut state, now, &Default::default())
        .await
        .unwrap();
    let changed = state.changed[&f.workspace.id];
    assert!(changed + 4 * HOUR > now, "files changed just now");
    assert!(state.changed_after(&f.workspace.id, changed - Duration::from_secs(1)));
    assert!(!state.changed_after(&f.workspace.id, changed));
    sweep_at(&*f.manager, &mut state, now + 5 * HOUR, &Default::default())
        .await
        .unwrap();
    assert!(!f.workspace.path.exists());
    sweep_at(&*f.manager, &mut state, now + 5 * HOUR, &Default::default())
        .await
        .unwrap();
    assert!(state.changed.is_empty(), "a removed workspace is forgotten");
}

#[tokio::test]
async fn a_future_modification_time_defers_walks_no_later_than_the_walk() {
    let f = Fixture::new().await;
    f.squash_merge();
    f.age_out_activity().await;
    let file = f.workspace.path.join("future");
    fs::write(&file, "").unwrap();
    let now = UnixTime::now();
    let future = std::time::SystemTime::now() + 1000 * HOUR;
    fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(future)
        .unwrap();
    let mut state = State::default();
    sweep_at(&*f.manager, &mut state, now, &Default::default())
        .await
        .unwrap();
    assert_eq!(state.changed[&f.workspace.id], now);
}

#[tokio::test]
async fn merged_cleanup_removes_a_chat_stopped_for_its_idle_period() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    (manager.write_local_config(&repo.id, Some(ENABLED.into())))
        .await
        .unwrap();
    let chat = manager
        .start_chat(repo.id.into(), "talk".into(), "claude".into(), None)
        .await
        .unwrap();
    let mut state = State::default();
    let now = UnixTime::now();
    // Running, it is in use.
    sweep_at(&*manager, &mut state, now + 300 * HOUR, &Default::default())
        .await
        .unwrap();
    assert_eq!(manager.spaces.chats().await.unwrap().len(), 1);
    manager.agents.end_executions(&*manager, &chat.id).await;
    // Stopped just now, it is not idle yet.
    sweep_at(&*manager, &mut state, now, &Default::default())
        .await
        .unwrap();
    assert_eq!(manager.spaces.chats().await.unwrap().len(), 1);
    sweep_at(&*manager, &mut state, now + 5 * HOUR, &Default::default())
        .await
        .unwrap();
    assert!(manager.spaces.chats().await.unwrap().is_empty());
}

/// The fixture's workspace, with its issue unlinked, watching a PR on a
/// forge that reports it `merged` or open, in a repository with
/// `[pr] watch_merge_runs` on, and its branch squashed onto the default
/// branch. Returns the PR's URL and the forge.
async fn watching_a_pr(f: &Fixture, merged: bool) -> (String, shoal_forge::test_support::Server) {
    f.unlink_issue().await;
    let server = shoal_forge::test_support::Server::start(move |request| {
        if request.line != "GET /api/v1/repos/team/repo/pulls/1" {
            return (404, Vec::new(), Vec::new());
        }
        let state = if merged { "closed" } else { "open" };
        let pull = serde_json::json!({"number": 1, "title": "Fix", "state": state,
            "merged": merged, "merge_commit_sha": merged.then_some("squash"),
            "head": {"ref": "topic", "sha": "head"}, "base": {"ref": "main"}});
        (200, Vec::new(), pull.to_string().into())
    });
    let host = &server.host;
    crate::forge::hosts::learn(host, crate::forge::backends::ForgeKind::Forgejo);
    crate::daemon::test_support::configure(
        &f.manager,
        &format!(
            "[machine.forges.\"{host}\"]\ntype = \"forgejo\"\nuser_token = \"user\"\n\n\
             [cleanup]\ngrace_minutes = 0\n\n[pr]\nwatch_merge_runs = true\n"
        ),
    )
    .await;
    let pr = format!("http://{host}/team/repo/pulls/1");
    let registration = crate::daemon::links::pr::Registration {
        kind: crate::daemon::links::pr::RegistrationKind::Watch {
            urls: vec![pr.parse().unwrap()],
            merged_head: None,
        },
        error: None,
    };
    let id = f.workspace.id.clone();
    (f.manager.store)
        .run(move |db| {
            db.execute(
                "INSERT INTO pr_cleanup(space_id,record) VALUES (?1,?2)",
                rusqlite::params![id, serde_json::to_string(&registration)?],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    f.squash_merge();
    (pr, server)
}

#[tokio::test]
async fn merged_cleanup_waits_for_a_poll_to_see_a_watched_prs_merge() {
    let f = Fixture::new().await;
    let (pr, _forge) = watching_a_pr(&f, true).await;
    assert!(f.kept_after(5).await, "the PR's merge is unseen");

    // The poll sees the merge, without a merge commit to look up runs of.
    let id = f.workspace.id.clone();
    (f.manager.store)
        .run(move |db| {
            db.execute(
                "INSERT INTO space_item_activity(space_id,url,record) VALUES (?1,?2,?3)",
                rusqlite::params![
                    id,
                    pr,
                    r#"{"comments":{},"checks":{},"conflict":null,"state":"merged","errors":{}}"#
                ],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(!f.kept_after(5).await);
}

#[tokio::test]
async fn a_watched_pr_left_open_does_not_keep_a_merged_workspace() {
    let f = Fixture::new().await;
    let _forge = watching_a_pr(&f, false).await;
    assert!(!f.kept_after(5).await);
}
