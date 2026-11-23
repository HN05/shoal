use crate::git;
use crate::model::CheckpointCause;
use crate::model::Workspace;
use crate::stand_in;
use crate::state::WorkspaceState;
use crate::workspace::checkpoints::*;
use rusqlite::params;
use shoal_core::test_support::{commit, git, repository};
use shoal_ids::checkpoint_numbers::CheckpointNumber;
use std::fs;

struct Fixture {
    manager: stand_in::Fixture,
    checkout: std::path::PathBuf,
    workspace: Workspace,
}

impl Fixture {
    async fn new() -> Self {
        let manager = stand_in::Fixture::new().await;
        let root = &manager.root;
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

    async fn set_expiry(&self, number: CheckpointNumber, expires_at: crate::time::UnixTime) {
        let id = self.workspace.id.clone();
        self.manager
            .store
            .run(move |db| {
                db.execute(
                    "UPDATE checkpoints SET expires_at=?3 WHERE workspace_id=?1 AND number=?2",
                    params![id, number, expires_at],
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
        .spaces
        .take_checkpoint(
            &**manager,
            &manager.named("topic").await,
            Some("before the change".into()),
        )
        .await
        .unwrap();
    assert_eq!(first.number, 1);
    assert_eq!(first.branch, Some("topic".into()));
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
        .spaces
        .take_checkpoint(&**manager, &manager.named("topic").await, None)
        .await
        .unwrap();
    assert_eq!(second.number, 2);
    f.set_expiry(2.into(), crate::time::UnixTime::from_seconds(1))
        .await;
    manager
        .spaces
        .checkpoint_after_agent(&**manager, &f.workspace.id)
        .await;
    let numbers = async || {
        manager
            .spaces
            .checkpoints(&manager.named("topic").await, None)
            .await
            .unwrap()
            .unwrap()
            .iter()
            .map(|checkpoint| checkpoint.number)
            .collect::<Vec<_>>()
    };
    assert_eq!(numbers().await, [1], "checkpoint 2 expired");
    f.write("scratch", "more notes\n");
    manager
        .spaces
        .checkpoint_after_agent(&**manager, &f.workspace.id)
        .await;
    manager
        .spaces
        .checkpoint_after_agent(&**manager, &f.workspace.id)
        .await;
    assert_eq!(numbers().await, [1, 3]);
    let listed = manager
        .spaces
        .checkpoints(&manager.named("topic").await, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(listed[1].cause, CheckpointCause::AgentStopped);
    assert!(
        manager
            .spaces
            .checkpoints(&manager.named("missing").await, None)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn the_archive_limit_caps_only_an_agents_checkpoint() {
    let f = Fixture::new().await;
    let manager = &f.manager;
    crate::stand_in::configure(manager, "[cleanup.restore]\narchive_max_mb = 0\n").await;
    f.write("scratch", "notes\n");
    let head = f.git(&["rev-parse", "HEAD"]);
    let requested = manager
        .spaces
        .take_checkpoint(&**manager, &manager.named("topic").await, None)
        .await
        .unwrap();
    assert!(!requested.files_omitted);

    manager
        .spaces
        .checkpoint_after_agent(&**manager, &f.workspace.id)
        .await;
    let id = f.workspace.id.clone();
    let agents = (manager.store.run(move |db| latest(db, &id)).await)
        .unwrap()
        .unwrap();
    assert_eq!(agents.cause, CheckpointCause::AgentStopped);
    assert!(agents.files_omitted);
    assert_eq!(agents.commit, head.trim());

    // A restore keeps the state it replaces whatever its size.
    f.write("scratch", "more notes\n");
    let restored = manager
        .spaces
        .restore_checkpoint(
            &**manager,
            &manager.named("topic").await,
            requested.number,
            None,
        )
        .await
        .unwrap();
    assert!(!restored.replaced.files_omitted);
    assert_eq!(f.read("scratch").as_deref(), Some("notes\n"));
    assert_eq!(
        git(
            &f.checkout,
            &["show", &format!("{}:scratch", restored.replaced.commit)]
        ),
        "more notes\n"
    );

    // No checkpoint keeps the files of a nested repository.
    repository(&f.workspace.path, "nested");
    let nested = manager
        .spaces
        .take_checkpoint(&**manager, &manager.named("topic").await, None)
        .await
        .unwrap();
    assert!(nested.files_omitted);
    assert_eq!(nested.commit, head.trim());
}

#[tokio::test]
async fn listings_come_in_pages_that_fit_a_protocol_frame() {
    let f = Fixture::new().await;
    let long = "n".repeat(NAME_LIMIT + 1);
    f.manager
        .spaces
        .take_checkpoint(&*f.manager, &f.manager.named("topic").await, Some(long))
        .await
        .unwrap_err();
    let checkpoint = f
        .manager
        .spaces
        .take_checkpoint(
            &*f.manager,
            &f.manager.named("topic").await,
            Some("n".repeat(NAME_LIMIT)),
        )
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
            .spaces
            .checkpoints(&f.manager.named("topic").await, after)
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
async fn agents_leave_no_checkpoint_while_another_lifecycle_operation_runs() {
    let f = Fixture::new().await;
    f.manager
        .spaces
        .reserve_lifecycle(&f.workspace.id, WorkspaceState::Removing)
        .await
        .unwrap();
    f.manager
        .spaces
        .checkpoint_after_agent(&*f.manager, &f.workspace.id)
        .await;
    f.manager
        .spaces
        .take_checkpoint(&*f.manager, &f.manager.named("topic").await, None)
        .await
        .unwrap_err();
    assert!(
        f.manager
            .spaces
            .checkpoints(&f.manager.named("topic").await, None)
            .await
            .unwrap()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn numbers_are_never_reused_after_checkpoints_expire() {
    let f = Fixture::new().await;
    let manager = &f.manager;
    manager
        .spaces
        .take_checkpoint(&**manager, &manager.named("topic").await, None)
        .await
        .unwrap();
    let second = manager
        .spaces
        .take_checkpoint(&**manager, &manager.named("topic").await, None)
        .await
        .unwrap();
    f.set_expiry(second.number, crate::time::UnixTime::from_seconds(0))
        .await;
    manager.spaces.expire_checkpoints(&**manager).await.unwrap();
    let third = manager
        .spaces
        .take_checkpoint(&**manager, &manager.named("topic").await, None)
        .await
        .unwrap();
    assert_eq!(third.number, 3);
    manager
        .spaces
        .restore_checkpoint(
            &**manager,
            &manager.named("topic").await,
            second.number,
            None,
        )
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
        .spaces
        .take_checkpoint(&**manager, &manager.named("topic").await, None)
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
        .spaces
        .restore_checkpoint(
            &**manager,
            &manager.named("topic").await,
            checkpoint.number,
            None,
        )
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
        .spaces
        .restore_checkpoint(
            &**manager,
            &manager.named("topic").await,
            restored.replaced.number,
            None,
        )
        .await
        .unwrap();
    assert_eq!(f.git(&["rev-parse", "HEAD"]), later);
    assert_eq!(f.git(&["status", "--porcelain"]), later_status);
    assert_eq!(f.read("untracked").as_deref(), Some("later\n"));

    manager
        .spaces
        .restore_checkpoint(&**manager, &manager.named("topic").await, 99.into(), None)
        .await
        .unwrap_err();
}

#[tokio::test]
async fn expiry_deletes_expired_and_unowned_checkpoints_and_adopts_removal_refs() {
    let f = Fixture::new().await;
    let manager = &f.manager;
    f.write("scratch", "notes\n");
    let first = manager
        .spaces
        .take_checkpoint(&**manager, &manager.named("topic").await, None)
        .await
        .unwrap();
    f.write("scratch", "more notes\n");
    let second = manager
        .spaces
        .take_checkpoint(&**manager, &manager.named("topic").await, None)
        .await
        .unwrap();

    // An expired checkpoint goes with its ref.
    f.set_expiry(first.number, crate::time::UnixTime::from_seconds(0))
        .await;
    manager.spaces.expire_checkpoints(&**manager).await.unwrap();
    let kept = [git::checkpoint::reference(&second.commit)];
    assert_eq!(checkpoint_refs(&f.checkout), kept);

    // A workspace that is gone without a removal record owns nothing.
    let id = f.workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute("DELETE FROM spaces WHERE id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();
    manager.spaces.expire_checkpoints(&**manager).await.unwrap();
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
async fn expiry_prunes_the_objects_of_deleted_checkpoints_as_gc_would() {
    let f = Fixture::new().await;
    let manager = &f.manager;
    git(&f.checkout, &["config", "gc.pruneExpire", "1.hour.ago"]);
    f.write("scratch", "expired notes\n");
    let expired = manager
        .spaces
        .take_checkpoint(&**manager, &manager.named("topic").await, None)
        .await
        .unwrap();
    let objects = f.checkout.join(".git/objects");
    let loose = |id: &str| objects.join(&id[..2]).join(&id[2..]);
    let commit = expired.commit.as_str();
    let kept = git(
        &f.checkout,
        &[
            "rev-parse",
            commit,
            &format!("{commit}^{{tree}}"),
            &format!("{commit}:scratch"),
        ],
    );
    let kept: Vec<_> = kept.lines().map(loose).collect();
    // Written before gc.pruneExpire, unlike an unreachable object written
    // since; Git also keeps what a recent object reaches.
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 3600);
    for object in &kept {
        fs::File::open(object).unwrap().set_modified(old).unwrap();
    }
    f.write("fresh", "fresh\n");
    let fresh = loose(f.git(&["hash-object", "-w", "fresh"]).trim());

    f.set_expiry(expired.number, crate::time::UnixTime::from_seconds(0))
        .await;
    manager.spaces.expire_checkpoints(&**manager).await.unwrap();
    assert!(checkpoint_refs(&f.checkout).is_empty());
    assert!(kept.iter().all(|object| !object.exists()));
    assert!(fresh.exists());
}

#[tokio::test]
async fn a_starting_checkpoint_outlives_its_expiry_while_its_workspace_exists() {
    let f = Fixture::new().await;
    let manager = &f.manager;
    f.write("scratch", "notes\n");
    let start = manager
        .spaces
        .take_checkpoint(&**manager, &manager.named("topic").await, None)
        .await
        .unwrap();
    let later = manager
        .spaces
        .take_checkpoint(&**manager, &manager.named("topic").await, None)
        .await
        .unwrap();
    let (id, number) = (f.workspace.id.clone(), start.number);
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
    manager.spaces.expire_checkpoints(&**manager).await.unwrap();
    let listed = manager
        .spaces
        .checkpoints(&manager.named("topic").await, None)
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
        .spaces
        .workspace(&manager.named("topic").await)
        .await
        .unwrap()
        .part
        .copied_from
        .unwrap();
    assert_eq!(copied.workspace, None, "the source is gone");
    assert_eq!(copied.starting_checkpoint, start.number);
    assert_eq!(
        (copied.head, copied.commit),
        (start.head.clone(), start.commit.clone())
    );
}

/// The checkpoint refs in the repository, sorted.
fn checkpoint_refs(repo: &std::path::Path) -> Vec<String> {
    let mut refs: Vec<String> = git(
        repo,
        &["for-each-ref", "--format=%(refname)", "refs/shoal/"],
    )
    .lines()
    .map(str::to_owned)
    .collect();
    refs.sort();
    refs
}
