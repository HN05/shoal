use std::{fs, sync::Arc};

use super::*;
use crate::{
    daemon::workspace::restore::tests::checkpoint_refs,
    test_support::{commit, git, manager, repository},
};

struct Fixture {
    _root: tempfile::TempDir,
    manager: Arc<Manager>,
    checkout: std::path::PathBuf,
    workspace: Workspace,
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
        Self {
            _root: root,
            manager,
            checkout,
            workspace,
        }
    }

    fn write(&self, file: &str, contents: &str) {
        fs::write(self.workspace.path.join(file), contents).unwrap();
    }

    fn read(&self, file: &str) -> Option<String> {
        fs::read_to_string(self.workspace.path.join(file)).ok()
    }

    fn git(&self, args: &[&str]) -> String {
        git(&self.workspace.path, args)
    }

    async fn set_expiry(&self, number: u64, expires_at: i64) {
        let id = self.workspace.id.clone();
        self.manager
            .store
            .run(move |db| {
                db.execute(
                    "UPDATE checkpoints SET expires_at=?3 WHERE workspace_id=?1 AND number=?2",
                    params![id, number as i64, expires_at],
                )?;
                Ok(())
            })
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn checkpoints_keep_head_and_unsaved_files_and_only_manual_ones_repeat() {
    let f = Fixture::new().await;
    let manager = &f.manager;
    f.write("scratch", "notes\n");
    let first = manager
        .take_checkpoint("topic", Some("before the change".into()))
        .await
        .unwrap();
    assert_eq!(first.number, 1);
    assert_eq!(first.name.as_deref(), Some("before the change"));
    assert_eq!(first.cause, CheckpointCause::Manual);
    assert_eq!(first.branch.as_deref(), Some("topic"));
    assert_eq!(first.head, f.git(&["rev-parse", "HEAD"]).trim());
    assert_eq!(
        git(&f.checkout, &["show", &format!("{}:scratch", first.commit)]),
        "notes\n"
    );
    assert_eq!(
        f.git(&["status", "--porcelain"]),
        "?? scratch\n",
        "the worktree is untouched"
    );

    // A manual checkpoint repeats an unchanged state; an automatic one
    // extends the latest unexpired one instead.
    let second = manager.take_checkpoint("topic", None).await.unwrap();
    assert_eq!(second.number, 2);
    f.set_expiry(2, 1).await;
    manager.checkpoint_after_agent(&f.workspace.id).await;
    let numbers = async || {
        manager
            .checkpoints("topic")
            .await
            .unwrap()
            .unwrap()
            .iter()
            .map(|checkpoint| checkpoint.number)
            .collect::<Vec<_>>()
    };
    assert_eq!(numbers().await, [1], "checkpoint 2 expired");
    f.write("scratch", "more notes\n");
    manager.checkpoint_after_agent(&f.workspace.id).await;
    manager.checkpoint_after_agent(&f.workspace.id).await;
    assert_eq!(numbers().await, [1, 3]);
    let listed = manager.checkpoints("topic").await.unwrap().unwrap();
    assert_eq!(listed[1].cause, CheckpointCause::AgentStopped);
    assert!(manager.checkpoints("missing").await.unwrap().is_none());
}

#[tokio::test]
async fn agents_leave_no_checkpoint_while_another_lifecycle_operation_runs() {
    let f = Fixture::new().await;
    f.manager
        .reserve_lifecycle(&f.workspace.id, WorkspaceState::Removing)
        .await
        .unwrap();
    f.manager.checkpoint_after_agent(&f.workspace.id).await;
    let error = f.manager.take_checkpoint("topic", None).await.unwrap_err();
    assert!(
        format!("{error:#}").contains("workspace is removing"),
        "{error:#}"
    );
    assert!(
        f.manager
            .checkpoints("topic")
            .await
            .unwrap()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn restore_returns_to_a_checkpoint_after_keeping_the_current_state() {
    let f = Fixture::new().await;
    let manager = &f.manager;
    f.write("tracked", "checkpointed\n");
    f.write("scratch", "notes\n");
    let checkpoint = manager.take_checkpoint("topic", None).await.unwrap();
    let status = f.git(&["status", "--porcelain"]);

    f.write("work", "work\n");
    commit(&f.workspace.path, "work");
    let later = f.git(&["rev-parse", "HEAD"]);
    f.write("tracked", "later\n");
    fs::remove_file(f.workspace.path.join("scratch")).unwrap();
    f.write("untracked", "later\n");
    let later_status = f.git(&["status", "--porcelain"]);

    let restored = manager
        .restore_checkpoint("topic", checkpoint.number, None)
        .await
        .unwrap();
    assert_eq!(restored.checkpoint, checkpoint);
    assert_eq!(restored.replaced.cause, CheckpointCause::Restore);
    assert_eq!(restored.workspace.state, WorkspaceState::Ready);
    assert_eq!(f.git(&["branch", "--show-current"]), "topic\n");
    assert_eq!(f.git(&["rev-parse", "HEAD"]).trim(), checkpoint.head);
    assert_eq!(f.git(&["status", "--porcelain"]), status);
    assert_eq!(f.read("scratch").as_deref(), Some("notes\n"));
    assert_eq!(f.read("untracked"), None);

    // The replaced state is a checkpoint like any other.
    manager
        .restore_checkpoint("topic", restored.replaced.number, None)
        .await
        .unwrap();
    assert_eq!(f.git(&["rev-parse", "HEAD"]), later);
    assert_eq!(f.git(&["status", "--porcelain"]), later_status);
    assert_eq!(f.read("untracked").as_deref(), Some("later\n"));

    let error = manager
        .restore_checkpoint("topic", 99, None)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("workspace topic has no checkpoint 99"),
        "{error:#}"
    );
}

#[tokio::test]
async fn restore_refuses_what_it_would_lose_or_disturb() {
    let f = Fixture::new().await;
    let manager = &f.manager;
    let checkpoint = manager.take_checkpoint("topic", None).await.unwrap();
    let refused = async |own: Option<&Caller>| {
        let error = manager
            .restore_checkpoint("topic", checkpoint.number, own)
            .await
            .unwrap_err();
        let workspace = manager.workspace("topic").await.unwrap();
        assert_eq!(workspace.state, WorkspaceState::Ready);
        format!("{error:#}")
    };

    // Another execution runs; the caller's own does not count.
    let id = f.workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO executions(id,workspace_id,state) VALUES ('agent',?1,'running')",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(refused(None).await.contains("execution agent runs"));
    let own = Caller {
        workspace_id: f.workspace.id.clone(),
        execution: Some(crate::daemon::scope::ScopedExecution {
            id: "agent".into(),
            kind: crate::daemon::workspace::ExecutionKind::Command,
        }),
    };
    manager
        .restore_checkpoint("topic", checkpoint.number, Some(&own))
        .await
        .unwrap();

    // Unsaved files no checkpoint can keep.
    repository(&f.workspace.path, "nested");
    assert!(
        refused(Some(&own))
            .await
            .contains("cannot keep the workspace's unsaved files")
    );
    fs::remove_dir_all(f.workspace.path.join("nested")).unwrap();

    // A Git operation under way.
    let git_dir = f.git(&["rev-parse", "--absolute-git-dir"]);
    let merge_head = std::path::Path::new(git_dir.trim()).join("MERGE_HEAD");
    fs::write(&merge_head, format!("{}\n", checkpoint.head)).unwrap();
    assert!(
        refused(Some(&own))
            .await
            .contains("Git operation is under way")
    );
    fs::remove_file(merge_head).unwrap();

    // Another branch.
    f.git(&["switch", "-q", "-c", "other"]);
    assert!(
        refused(Some(&own))
            .await
            .contains("checkpoint 1 is on branch topic; the workspace is on branch other")
    );
}

#[tokio::test]
async fn expiry_deletes_expired_and_unowned_checkpoints_and_adopts_removal_refs() {
    let f = Fixture::new().await;
    let manager = &f.manager;
    f.write("scratch", "notes\n");
    let first = manager.take_checkpoint("topic", None).await.unwrap();
    f.write("scratch", "more notes\n");
    let second = manager.take_checkpoint("topic", None).await.unwrap();

    // An expired checkpoint goes with its ref; one extended after expiry
    // selected it stays.
    f.set_expiry(first.number, 0).await;
    manager.expire_checkpoints().await.unwrap();
    assert_eq!(
        checkpoint_refs(&f.checkout),
        [git::checkpoint::reference(&f.workspace.id, second.number)]
    );
    let (id, commit) = (f.workspace.id.clone(), second.commit.clone());
    let deleted = manager
        .store
        .run(move |db| delete_unless_extended(db, &id, second.number, &commit))
        .await
        .unwrap();
    assert!(!deleted);

    // A removal snapshot from before checkpoints moves to the checkpoint the
    // store migration recorded for it.
    let legacy = format!("refs/shoal/removed/{}", f.workspace.id);
    git(&f.checkout, &["update-ref", &legacy, &second.commit]);
    let reference = git::checkpoint::reference(&f.workspace.id, second.number);
    git(&f.checkout, &["update-ref", "-d", &reference]);
    manager.expire_checkpoints().await.unwrap();
    assert_eq!(
        checkpoint_refs(&f.checkout),
        [git::checkpoint::reference(&f.workspace.id, second.number)]
    );

    // A workspace that is gone without a removal record owns nothing.
    let id = f.workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute("DELETE FROM workspaces WHERE id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();
    manager.expire_checkpoints().await.unwrap();
    assert!(checkpoint_refs(&f.checkout).is_empty());
    let id = f.workspace.id.clone();
    let left = manager
        .store
        .run(move |db| workspace_checkpoints(db, &id))
        .await
        .unwrap();
    assert!(left.is_empty());
}
