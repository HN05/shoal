use std::{fs, sync::Arc};

use super::*;
use crate::client::space_names::SpaceSelector;
use crate::daemon::{
    test_support::{commit, git, manager, repository},
    workspace::restore::tests::checkpoint_refs,
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

    async fn set_expiry(&self, number: u64, expires_at: crate::time::UnixTime) {
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
        .take_checkpoint(&"topic".into(), Some("before the change".into()))
        .await
        .unwrap();
    assert_eq!(first.number, 1);
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
    let second = manager
        .take_checkpoint(&"topic".into(), None)
        .await
        .unwrap();
    assert_eq!(second.number, 2);
    f.set_expiry(2, crate::time::UnixTime::from_seconds(1))
        .await;
    manager.checkpoint_after_agent(&f.workspace.id).await;
    let numbers = async || {
        manager
            .checkpoints(&"topic".into(), None)
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
    let listed = manager
        .checkpoints(&"topic".into(), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(listed[1].cause, CheckpointCause::AgentStopped);
    assert!(
        manager
            .checkpoints(&"missing".into(), None)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn listings_come_in_pages_that_fit_a_protocol_frame() {
    let f = Fixture::new().await;
    let long = "n".repeat(NAME_LIMIT + 1);
    f.manager
        .take_checkpoint(&"topic".into(), Some(long))
        .await
        .unwrap_err();
    let checkpoint = f
        .manager
        .take_checkpoint(&"topic".into(), Some("n".repeat(NAME_LIMIT)))
        .await
        .unwrap();
    let record = checkpoint.clone();
    f.manager
        .store
        .write(move |tx| {
            for _ in 0..300 {
                insert_numbered(tx, &record)?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let mut numbers = Vec::new();
    let mut after = None;
    loop {
        let page = f
            .manager
            .checkpoints(&"topic".into(), after)
            .await
            .unwrap()
            .unwrap();
        let Some(last) = page.last() else { break };
        let size = serde_json::to_vec(&page).unwrap().len();
        assert!(size <= PAGE_BYTES, "{size} bytes");
        after = Some(last.number);
        numbers.extend(page.iter().map(|c| c.number));
    }
    assert_eq!(numbers, (1..=301).collect::<Vec<_>>());
}

#[tokio::test]
async fn an_agent_exit_leaves_a_checkpoint_only_once_its_processes_are_proven_stopped() {
    let f = Fixture::new().await;
    f.write("scratch", "notes\n");
    let count = async || {
        f.manager
            .checkpoints(&"topic".into(), None)
            .await
            .unwrap()
            .unwrap()
            .len()
    };
    f.manager
        .post_agent_exit(&f.workspace, &"claude".into(), Some(0), false)
        .await;
    assert_eq!(count().await, 0);
    f.manager
        .post_agent_exit(&f.workspace, &"claude".into(), Some(0), true)
        .await;
    assert_eq!(count().await, 1);
}

#[tokio::test]
async fn agents_leave_no_checkpoint_while_another_lifecycle_operation_runs() {
    let f = Fixture::new().await;
    f.manager
        .reserve_lifecycle(&f.workspace.id, WorkspaceState::Removing)
        .await
        .unwrap();
    f.manager.checkpoint_after_agent(&f.workspace.id).await;
    f.manager
        .take_checkpoint(&"topic".into(), None)
        .await
        .unwrap_err();
    assert!(
        f.manager
            .checkpoints(&"topic".into(), None)
            .await
            .unwrap()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn links_cannot_change_while_a_checkpoint_is_restored() {
    use crate::{forge::items::link::LinkKind, model::Relation};
    let f = Fixture::new().await;
    git(
        &f.checkout,
        &["remote", "add", "origin", "https://github.com/acme/repo"],
    );
    f.manager
        .reserve_lifecycle(&f.workspace.id, WorkspaceState::Restoring)
        .await
        .unwrap();
    f.manager
        .link_item(
            &SpaceSelector::id(&f.workspace.id),
            LinkKind::Pr,
            "5",
            Relation::Related,
            None,
            false,
        )
        .await
        .unwrap_err();
}

#[tokio::test]
async fn numbers_are_never_reused_after_checkpoints_expire() {
    let f = Fixture::new().await;
    let manager = &f.manager;
    manager
        .take_checkpoint(&"topic".into(), None)
        .await
        .unwrap();
    let second = manager
        .take_checkpoint(&"topic".into(), None)
        .await
        .unwrap();
    f.set_expiry(second.number, crate::time::UnixTime::from_seconds(0))
        .await;
    manager.expire_checkpoints().await.unwrap();
    let third = manager
        .take_checkpoint(&"topic".into(), None)
        .await
        .unwrap();
    assert_eq!(third.number, 3);
    manager
        .restore_checkpoint(&"topic".into(), second.number, None)
        .await
        .unwrap_err();
}

#[tokio::test]
async fn restore_returns_to_a_checkpoint_after_keeping_the_current_state() {
    let f = Fixture::new().await;
    let manager = &f.manager;
    f.write("tracked", "checkpointed\n");
    f.write("scratch", "notes\n");
    let checkpoint = manager
        .take_checkpoint(&"topic".into(), None)
        .await
        .unwrap();
    let status = f.git(&["status", "--porcelain"]);

    f.write("work", "work\n");
    commit(&f.workspace.path, "work");
    let later = f.git(&["rev-parse", "HEAD"]);
    f.write("tracked", "later\n");
    fs::remove_file(f.workspace.path.join("scratch")).unwrap();
    f.write("untracked", "later\n");
    let later_status = f.git(&["status", "--porcelain"]);

    let restored = manager
        .restore_checkpoint(&"topic".into(), checkpoint.number, None)
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
        .restore_checkpoint(&"topic".into(), restored.replaced.number, None)
        .await
        .unwrap();
    assert_eq!(f.git(&["rev-parse", "HEAD"]), later);
    assert_eq!(f.git(&["status", "--porcelain"]), later_status);
    assert_eq!(f.read("untracked").as_deref(), Some("later\n"));

    manager
        .restore_checkpoint(&"topic".into(), 99, None)
        .await
        .unwrap_err();
}

#[tokio::test]
async fn restore_refuses_what_it_would_lose_or_disturb() {
    let f = Fixture::new().await;
    let manager = &f.manager;
    let checkpoint = manager
        .take_checkpoint(&"topic".into(), None)
        .await
        .unwrap();
    let refused = async |own: Option<&Caller>| {
        manager
            .restore_checkpoint(&"topic".into(), checkpoint.number, own)
            .await
            .unwrap_err();
        let workspace = manager.workspace(&"topic".into()).await.unwrap();
        assert_eq!(workspace.state, WorkspaceState::Ready);
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
    refused(None).await;
    let own = Caller {
        space_id: f.workspace.id.clone(),
        execution: Some(crate::daemon::scope::ScopedExecution {
            id: "agent".into(),
            kind: crate::daemon::workspace::ExecutionKind::Command,
            agent_execution: None,
        }),
    };
    manager
        .restore_checkpoint(&"topic".into(), checkpoint.number, Some(&own))
        .await
        .unwrap();

    // Unsaved files no checkpoint can keep.
    repository(&f.workspace.path, "nested");
    refused(Some(&own)).await;
    fs::remove_dir_all(f.workspace.path.join("nested")).unwrap();

    // A Git operation under way.
    let git_dir = f.git(&["rev-parse", "--absolute-git-dir"]);
    let merge_head = std::path::Path::new(git_dir.trim()).join("MERGE_HEAD");
    fs::write(&merge_head, format!("{}\n", checkpoint.head)).unwrap();
    refused(Some(&own)).await;
    fs::remove_file(merge_head).unwrap();

    // Another branch.
    f.git(&["switch", "-q", "-c", "other"]);
    refused(Some(&own)).await;
}

#[tokio::test]
async fn expiry_deletes_expired_and_unowned_checkpoints_and_adopts_removal_refs() {
    let f = Fixture::new().await;
    let manager = &f.manager;
    f.write("scratch", "notes\n");
    let first = manager
        .take_checkpoint(&"topic".into(), None)
        .await
        .unwrap();
    f.write("scratch", "more notes\n");
    let second = manager
        .take_checkpoint(&"topic".into(), None)
        .await
        .unwrap();

    // An expired checkpoint goes with its ref.
    f.set_expiry(first.number, crate::time::UnixTime::from_seconds(0))
        .await;
    manager.expire_checkpoints().await.unwrap();
    let kept = [git::checkpoint::reference(&second.commit)];
    assert_eq!(checkpoint_refs(&f.checkout), kept);

    // A removal snapshot from before checkpoints moves to the checkpoint the
    // store migration recorded for it.
    let legacy = format!("refs/shoal/removed/{}", f.workspace.id);
    git(&f.checkout, &["update-ref", &legacy, &second.commit]);
    git(&f.checkout, &["update-ref", "-d", &kept[0]]);
    manager.expire_checkpoints().await.unwrap();
    assert_eq!(checkpoint_refs(&f.checkout), kept);

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

#[tokio::test]
async fn a_starting_checkpoint_outlives_its_expiry_while_its_workspace_exists() {
    let f = Fixture::new().await;
    let manager = &f.manager;
    f.write("scratch", "notes\n");
    let start = manager
        .take_checkpoint(&"topic".into(), None)
        .await
        .unwrap();
    let later = manager
        .take_checkpoint(&"topic".into(), None)
        .await
        .unwrap();
    let (id, number) = (f.workspace.id.clone(), start.number as i64);
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE workspaces SET copied_from='removed',starting_checkpoint=?2 WHERE id=?1",
                params![id, number],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    f.set_expiry(start.number, crate::time::UnixTime::from_seconds(1))
        .await;
    f.set_expiry(later.number, crate::time::UnixTime::from_seconds(1))
        .await;
    manager.expire_checkpoints().await.unwrap();
    let listed = manager
        .checkpoints(&"topic".into(), None)
        .await
        .unwrap()
        .unwrap();
    let numbers: Vec<_> = listed.iter().map(|checkpoint| checkpoint.number).collect();
    assert_eq!(numbers, [start.number]);
    assert_eq!(
        checkpoint_refs(&f.checkout),
        [git::checkpoint::reference(&start.commit)]
    );
    let copied = manager
        .workspace(&"topic".into())
        .await
        .unwrap()
        .copied_from
        .unwrap();
    assert_eq!(copied.workspace, None, "the source is gone");
    assert_eq!(copied.starting_checkpoint, start.number);
    assert_eq!((copied.head, copied.commit), (start.head, start.commit));
}
