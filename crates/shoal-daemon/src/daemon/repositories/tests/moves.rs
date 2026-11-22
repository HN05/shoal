use anyhow::{Context, Result};
use rusqlite::params;
use std::path::{Path, PathBuf};

use crate::daemon::manager::Manager;
use crate::daemon::{
    executions::ExecutionKind, links::proposals::tests as proposals, test_support,
    workspace_files::Attacher,
};
use crate::model::{Repository, Workspace};
use crate::removal::{InspectionPolicy, UnsavedWork};
use crate::state::{QueueKind, QueueState, WorkspaceState};
use crate::time::UnixTime;
use crate::{fsutil, git};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::assignments::Done;
use shoal_protocol::space_names::SpaceSelector;
use shoal_repositories::spotlight::Layout;
use std::fs;
use std::sync::Arc;

/// A manager with a cloned repository and one workspace under the default
/// root, configured to move them into `<temp>/new`.
async fn cloned() -> Result<(
    tempfile::TempDir,
    Arc<Manager>,
    Repository,
    Workspace,
    PathBuf,
)> {
    let (temp, manager) = test_support::manager().await;
    let origin = test_support::repository(temp.path(), "origin");
    let repo = manager
        .repositories
        .register(format!("file://{}", origin.display()).into(), None, None)
        .await?;
    let workspace = manager
        .create_workspace(&repo.id, "feature".into(), None, None, None)
        .await?;
    let root = fs::canonicalize(temp.path())?.join("new");
    test_support::configure(&manager, &format!("[machine]\nroot_dir = {:?}\n", root)).await;
    Ok((temp, manager, repo, workspace, root))
}

#[tokio::test]
async fn moves_the_clone_and_workspaces_into_the_new_root() -> Result<()> {
    let (_temp, manager, repo, workspace, root) = cloned().await?;
    let old = repo.workspaces_dir.clone();
    // The seed's locked worktree moves and reconnects too.
    test_support::git(
        &repo.path,
        &["worktree", "add", "--lock", "--detach", "../.seed/worktree"],
    );
    manager
        .write_local_config(&repo.id, Some("[ports.web]\nport = 3000\n".into()))
        .await?;
    // So does the workspace's temporary directory.
    let tmp_dir = crate::tmp_dirs::directory(&repo, &workspace.id)?;
    fs::write(tmp_dir.join("scratch"), "garbage")?;

    let moved = manager
        .repositories
        .move_all(&*manager, Layout::Regular)
        .await?;
    let [moved] = moved.as_slice() else {
        anyhow::bail!("expected one move: {moved:?}");
    };
    let directory = root.join("origin");
    assert_eq!(moved.from, old);
    assert_eq!(moved.workspace_paths, std::slice::from_ref(&workspace.path));
    assert_eq!(moved.repository.workspaces_dir, directory);
    assert_eq!(moved.repository.path, directory.join(".checkout"));
    assert!(!old.exists());
    // The local config moves with the repository's directory.
    assert!(directory.join(".shoal/config.toml").exists());
    assert_eq!(
        manager.repositories.local_config_path(&repo.id).await?,
        directory.join(".shoal/config.toml")
    );

    let workspace = manager.workspace_by_id(&workspace.id).await?;
    assert_eq!(workspace.path, directory.join("feature"));
    assert_eq!(workspace.state, WorkspaceState::Ready);
    assert!(
        workspace
            .checkout
            .git_dir()
            .is_some_and(|dir| dir.starts_with(&directory))
    );
    manager.verify_worktree(&workspace).await?;
    let exported = manager
        .space_environment(&SpaceSelector::id(&workspace.id))
        .await?;
    let tmp_dir = directory.join(".tmp").join(&workspace.id);
    assert_eq!(exported[crate::env::TMPDIR], tmp_dir.to_string_lossy());
    assert!(tmp_dir.join("scratch").is_file());
    let trees = git::worktrees(&moved.repository.path, git::run).await?;
    for path in [&workspace.path, &directory.join(".seed/worktree")] {
        assert!(
            trees
                .iter()
                .any(|tree| tree.path == *path && !tree.prunable),
            "{} is not connected",
            path.display()
        );
    }

    // Moving again finds nothing outside the root, and new workspaces go there.
    assert!(
        manager
            .repositories
            .move_all(&*manager, Layout::Regular)
            .await?
            .is_empty()
    );
    let next = manager
        .create_workspace(&repo.id, "next".into(), None, None, None)
        .await?;
    assert_eq!(next.path, directory.join("next"));
    Ok(())
}

#[tokio::test]
async fn an_interrupted_rename_finishes_and_an_incomplete_copy_rolls_back() -> Result<()> {
    let (_temp, manager, repo, workspace, root) = cloned().await?;
    let source = repo.workspaces_dir.clone();
    let destination = root.join("origin");
    fs::create_dir_all(&destination)?;

    // A copy cut short leaves the source, so the move rolls back.
    manager
        .repositories
        .record_move_intent(&repo.id, &source, &destination)
        .await?;
    let reserved = manager
        .reserve_lifecycle(&workspace.id, WorkspaceState::Removing)
        .await;
    assert!(reserved.is_err());
    let chat = manager
        .start_chat(
            repo.id.clone().into(),
            "chat-1".into(),
            "codex".into(),
            None,
        )
        .await;
    assert!(chat.is_err());
    assert!(
        manager
            .repositories
            .remove(&*manager, &repo.id)
            .await
            .is_err()
    );
    // Quarantine fails the reserved workspaces; setup must not revive one.
    crate::daemon::quarantine::interrupted_operations(&manager.store).await?;
    test_support::configure(
        &manager,
        &format!("hooks.pre_setup = \"true\"\n[machine]\nroot_dir = {root:?}\n"),
    )
    .await;
    let setup = manager
        .agents
        .begin_execution(
            &*manager,
            workspace.id.as_str(),
            None,
            ExecutionKind::Setup,
            None,
        )
        .await;
    assert!(setup.is_err());
    fs::write(destination.join("partial"), "")?;
    manager.repositories.finish_moves(&*manager).await?;
    assert!(!destination.exists());
    assert_eq!(
        manager.repositories.repository(&repo.id).await?.path,
        repo.path
    );
    assert_eq!(
        manager.workspace_by_id(&workspace.id).await?.state,
        WorkspaceState::Ready
    );

    // A completed rename finishes when the daemon starts again, leaving a
    // directory created at the old path since.
    fs::create_dir_all(&destination)?;
    manager
        .repositories
        .record_move_intent(&repo.id, &source, &destination)
        .await?;
    fs::rename(&source, &destination)?;
    fs::create_dir(&source)?;
    fs::write(source.join("unrelated"), "keep")?;
    crate::daemon::quarantine::interrupted_operations(&manager.store).await?;
    manager.repositories.finish_moves(&*manager).await?;
    let workspace = manager.workspace_by_id(&workspace.id).await?;
    assert_eq!(workspace.path, destination.join("feature"));
    assert_eq!(workspace.state, WorkspaceState::Ready);
    assert_eq!(workspace.error, None);
    manager.verify_worktree(&workspace).await?;
    assert_eq!(fs::read_to_string(source.join("unrelated"))?, "keep");
    Ok(())
}

#[tokio::test]
async fn a_copied_move_reconnects_worktrees_before_deleting_the_original() -> Result<()> {
    let (_temp, manager, repo, workspace, root) = cloned().await?;
    let source = repo.workspaces_dir.clone();
    let destination = root.join("origin");
    fs::create_dir_all(&destination)?;

    // What a move to another filesystem leaves before the original goes.
    manager
        .repositories
        .record_move_intent(&repo.id, &source, &destination)
        .await?;
    fsutil::copy::copy_directory(&source, &destination).await?;
    manager
        .repositories
        .switch_move_records(&repo.id, &source, &destination)
        .await?;
    // An unavailable copy keeps the original.
    let away = root.join("unmounted");
    fs::rename(&destination, &away)?;
    assert!(manager.repositories.finish_moves(&*manager).await.is_err());
    assert!(source.join(".checkout").exists());
    fs::rename(&away, &destination)?;
    manager.repositories.finish_moves(&*manager).await?;

    assert!(!source.exists());
    let workspace = manager.workspace_by_id(&workspace.id).await?;
    assert_eq!(workspace.path, destination.join("feature"));
    assert!(
        workspace
            .checkout
            .git_dir()
            .is_some_and(|dir| dir.starts_with(&destination))
    );
    manager.verify_worktree(&workspace).await?;
    assert_eq!(
        git::head_branch(&workspace.path).await?,
        Some("feature".into())
    );
    let checkout = manager.repositories.repository(&repo.id).await?.path;
    let trees = git::worktrees(&checkout, git::run).await?;
    assert!(
        trees
            .iter()
            .any(|tree| tree.path == workspace.path && !tree.prunable)
    );
    Ok(())
}

#[tokio::test]
async fn a_checkpoint_waits_for_the_move_and_refuses_while_it_is_pending() -> Result<()> {
    let (_temp, manager, repo, workspace, root) = cloned().await?;
    let source = repo.workspaces_dir.clone();
    let destination = root.join("origin");
    fs::create_dir_all(&destination)?;

    // The move holds the repository's Git gate from its check to its end.
    let gate = manager.gates.lock_git(&repo.id).await;
    let checkpointing = manager.clone();
    let id = workspace.id.clone();
    let mut checkpoint = Box::pin(async move {
        checkpointing
            .take_checkpoint(&SpaceSelector::id(&id), None)
            .await
    });
    test_support::assert_pending(&mut checkpoint).await;
    manager
        .repositories
        .record_move_intent(&repo.id, &source, &destination)
        .await?;
    drop(gate);
    assert!(checkpoint.await.is_err());

    manager.repositories.finish_moves(&*manager).await?;
    manager
        .take_checkpoint(&SpaceSelector::id(&workspace.id), None)
        .await?;
    Ok(())
}

#[tokio::test]
async fn the_destination_avoids_a_deleted_workspace_recorded_there() -> Result<()> {
    let (_temp, manager, repo, _workspace, root) = cloned().await?;
    // Where the moved workspace `feature` would otherwise land.
    let placed = root.join("origin/feature");
    manager
        .create_workspace(&repo.id, "placed".into(), None, None, Some(placed.clone()))
        .await?;
    fs::remove_dir_all(root.join("origin"))?;

    let moved = manager
        .repositories
        .move_all(&*manager, Layout::Regular)
        .await?;
    let [moved] = moved.as_slice() else {
        anyhow::bail!("expected one move: {moved:?}");
    };
    assert_eq!(moved.repository.workspaces_dir, root.join("origin-2"));
    Ok(())
}

#[tokio::test]
async fn a_move_destination_and_workspace_paths_exclude_each_other() -> Result<()> {
    let (temp, manager, repo, _workspace, root) = cloned().await?;
    let source = repo.workspaces_dir.clone();

    // A workspace placed in the destination after its reservation keeps
    // the move from being recorded.
    let taken = root.join("origin");
    fs::create_dir_all(&taken)?;
    manager
        .create_workspace(
            &repo.id,
            "placed".into(),
            None,
            None,
            Some(taken.join("placed")),
        )
        .await?;
    assert!(
        manager
            .repositories
            .record_move_intent(&repo.id, &source, &taken)
            .await
            .is_err()
    );

    // While a move is recorded, no repository places a workspace there.
    let destination = root.join("origin-2");
    fs::create_dir_all(&destination)?;
    manager
        .repositories
        .record_move_intent(&repo.id, &source, &destination)
        .await?;
    let other = test_support::repository(temp.path(), "other");
    let other = manager
        .repositories
        .register(other.to_str().context("path")?.into(), None, None)
        .await?;
    let intruder = destination.join("intruder");
    let placed = manager
        .create_workspace(
            &other.id,
            "intruder".into(),
            None,
            None,
            Some(intruder.clone()),
        )
        .await;
    assert!(placed.is_err());
    assert!(!intruder.exists());
    manager.repositories.finish_moves(&*manager).await?;
    Ok(())
}

#[tokio::test]
async fn a_replaced_worktree_keeps_the_move_unfinished() -> Result<()> {
    let (_temp, manager, repo, workspace, root) = cloned().await?;
    let source = repo.workspaces_dir.clone();
    let destination = root.join("origin");
    fs::create_dir_all(&destination)?;
    manager
        .repositories
        .record_move_intent(&repo.id, &source, &destination)
        .await?;
    fs::rename(&source, &destination)?;
    let admin = destination.join(".checkout/.git/worktrees/feature");
    shoal_daemon_base::ownership::mark_owner(&admin, "another")?;

    assert!(manager.repositories.finish_moves(&*manager).await.is_err());
    assert!(
        manager
            .repositories
            .ensure_available(&repo.id)
            .await
            .is_err()
    );
    assert_eq!(
        manager.workspace_by_id(&workspace.id).await?.state,
        WorkspaceState::Reconciling
    );
    Ok(())
}

#[tokio::test]
async fn a_checkout_placed_in_its_directory_moves_with_its_source() -> Result<()> {
    let (temp, manager) = test_support::manager().await;
    let directory = temp.path().join("shoal/project");
    fs::create_dir_all(&directory)?;
    let checkout = test_support::repository(&directory, "checkout");
    let repo = manager
        .repositories
        .register(checkout.to_str().context("path")?.into(), None, None)
        .await?;
    assert_eq!(repo.workspaces_dir, fs::canonicalize(&directory)?);
    let root = fs::canonicalize(temp.path())?.join("new");
    test_support::configure(&manager, &format!("[machine]\nroot_dir = {:?}\n", root)).await;

    // Another checkout registered inside the directory keeps it in place.
    let other = test_support::repository(&directory, "other");
    let other = manager
        .repositories
        .register(other.to_str().context("path")?.into(), None, None)
        .await?;
    assert!(
        manager
            .repositories
            .move_all(&*manager, Layout::Regular)
            .await
            .is_err()
    );
    manager.repositories.remove(&*manager, &other.id).await?;

    manager
        .repositories
        .move_all(&*manager, Layout::Regular)
        .await?;
    let moved = manager.repositories.repository(&repo.id).await?;
    let expected = root.join("project/checkout");
    assert_eq!(moved.path, expected);
    assert_eq!(Path::new(&moved.source), expected);
    Ok(())
}

#[tokio::test]
async fn an_excluded_move_empties_the_indexed_root_and_links_it() -> Result<()> {
    let (temp, manager) = test_support::indexed_manager().await;
    let origin = test_support::repository(temp.path(), "origin");
    let repo = manager
        .repositories
        .register(format!("file://{}", origin.display()).into(), None, None)
        .await?;
    let workspace = manager
        .create_workspace(&repo.id, "feature".into(), None, None, None)
        .await?;
    let old = repo.workspaces_dir.clone();
    let root = old.parent().context("no root")?.to_owned();
    let excluded = root.with_file_name(".shoal.noindex");

    let plan = manager
        .repositories
        .plan_moves(&*manager, None, Layout::Excluded)
        .await?;
    assert_eq!(plan.directory, excluded);
    assert!(fs::symlink_metadata(&root)?.is_dir());
    fs::write(root.join("notes"), "")?;
    assert!(
        manager
            .repositories
            .move_all(&*manager, Layout::Excluded)
            .await
            .is_err()
    );
    assert!(old.exists());

    fs::remove_file(root.join("notes"))?;
    let moved = manager
        .repositories
        .move_all(&*manager, Layout::Excluded)
        .await?;
    let [moved] = moved.as_slice() else {
        anyhow::bail!("expected one move: {moved:?}");
    };
    let directory = excluded.join("origin");
    assert_eq!(moved.repository.workspaces_dir, directory);
    assert_eq!(fs::read_link(&root)?, Path::new(".shoal.noindex"));
    let workspace = manager.workspace_by_id(&workspace.id).await?;
    assert_eq!(workspace.path, directory.join("feature"));
    manager.verify_worktree(&workspace).await?;

    // A regular directory replaces the link and takes the repository back.
    let moved = manager
        .repositories
        .move_all(&*manager, Layout::Regular)
        .await?;
    let [moved] = moved.as_slice() else {
        anyhow::bail!("expected one move: {moved:?}");
    };
    assert_eq!(moved.repository.workspaces_dir, root.join("origin"));
    assert!(fs::symlink_metadata(&root)?.is_dir());
    Ok(())
}

#[tokio::test]
async fn an_excluded_move_refuses_to_link_a_recorded_workspace_path() -> Result<()> {
    let (temp, manager) = test_support::indexed_manager().await;
    let origin = test_support::repository(temp.path(), "origin");
    let repo = manager
        .repositories
        .register(format!("file://{}", origin.display()).into(), None, None)
        .await?;
    let placed = fs::canonicalize(temp.path())?.join("placed");
    manager
        .create_workspace(&repo.id, "placed".into(), None, None, Some(placed.clone()))
        .await?;
    fs::remove_dir_all(&placed)?;
    test_support::configure(&manager, &format!("[machine]\nroot_dir = {:?}\n", placed)).await;
    assert!(
        manager
            .repositories
            .move_all(&*manager, Layout::Excluded)
            .await
            .is_err()
    );
    assert!(fs::symlink_metadata(&placed).is_err());
    Ok(())
}

#[tokio::test]
async fn messages_and_files_follow_their_copies_to_the_new_root() -> Result<()> {
    let (temp, manager, _repo, workspace, root) = cloned().await?;
    let file = temp.path().join("crash.log");
    fs::write(&file, "panic")?;
    manager
        .send_space_message(
            None,
            &SpaceSelector::id(&workspace.id),
            "See the log".into(),
            vec![file.clone().into()],
        )
        .await?;
    manager
        .mark_space_done(
            &SpaceSelector::id(&workspace.id),
            Done {
                results: vec![file.into()],
                ..Done::default()
            },
            &Attacher::User,
        )
        .await?;

    manager
        .repositories
        .move_all(&*manager, Layout::Regular)
        .await?;
    let completion = manager
        .completion(&workspace.id)
        .await?
        .context("no completion")?;
    assert_eq!(completion.results[0].name, "crash.log");
    let messages = manager
        .agents
        .agent_messages(&*manager, &SpaceSelector::id(&workspace.id))
        .await?;
    let attachments = messages[0].attachments();
    let [copy] = attachments.as_slice() else {
        anyhow::bail!("expected one attachment: {messages:?}");
    };
    assert!(
        copy.starts_with(root.join("origin").join(".attachments")),
        "{copy:?}"
    );
    assert_eq!(fs::read_to_string(copy)?, "panic");
    let files = manager
        .attached_files(&SpaceSelector::id(&workspace.id))
        .await?;
    assert_eq!(files[0].path().unwrap(), *copy);
    let result = &files[1].path().unwrap();
    assert!(result.starts_with(root.join("origin")), "{result:?}");
    assert_eq!(fs::read_to_string(result)?, "panic");
    Ok(())
}

#[tokio::test]
async fn pending_proposals_follow_their_copies_to_the_new_root() -> Result<()> {
    let (_temp, manager, repo, workspace, root) = cloned().await?;
    let (body, copy) = proposals::kept_copy(&manager, &workspace, &repo.id, "shot.png").await;
    let pull = proposals::store_pull_proposal(&manager, &workspace, body, vec![copy.clone()]).await;

    manager
        .repositories
        .move_all(&*manager, Layout::Regular)
        .await?;
    let moved = manager
        .approval_request(pull.number)
        .await?
        .context("no proposal")?;
    let proposed = &moved.proposed_pull().context("no PR")?.pull;
    let path = &proposed.attachments[0].path;
    assert!(
        path.starts_with(root.join("origin").join(".proposals")),
        "{path:?}"
    );
    assert_eq!(fs::read_to_string(path)?, "shot.png");
    let shown = &proposed.body;
    assert!(shown.contains(&path.display().to_string()), "{shown}");
    Ok(())
}

#[tokio::test]
async fn a_move_waits_for_a_send_keeping_attached_files() -> Result<()> {
    let (temp, manager, repo, workspace, root) = cloned().await?;
    let file = temp.path().join("crash.log");
    fs::write(&file, "panic")?;
    // A send is copying files at the old place.
    let sending = manager.gates.lock_message_files(&repo.id).await;
    let gate = manager.gates.message_files_gate(&repo.id);
    let moving = tokio::spawn({
        let manager = manager.clone();
        async move {
            manager
                .repositories
                .move_all(&*manager, Layout::Regular)
                .await
        }
    });
    // The send, this test and the waiting move.
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        while Arc::strong_count(&gate) < 3 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .context("the move reaches the gate")?;
    let old = repo.workspaces_dir.clone();
    assert!(old.exists(), "the move waits before moving the directory");
    drop(sending);
    moving.await??;
    // A send after the move copies to the new place.
    manager
        .send_space_message(
            None,
            &SpaceSelector::id(&workspace.id),
            "See the log".into(),
            vec![file.into()],
        )
        .await?;
    let messages = manager
        .agents
        .agent_messages(&*manager, &SpaceSelector::id(&workspace.id))
        .await?;
    let copy = &messages[0].attachments()[0];
    assert!(copy.starts_with(root.join("origin")), "{copy:?}");
    assert!(copy.exists());
    Ok(())
}

#[tokio::test]
async fn a_plan_lists_running_executions_that_the_move_refuses() -> Result<()> {
    let (_temp, manager, repo, workspace, _root) = cloned().await?;
    let mut wrapper = tokio::process::Command::new("sleep")
        .arg("600")
        .current_dir(&workspace.path)
        .kill_on_drop(true)
        .spawn()?;
    let identity = crate::process::identity::capture(wrapper.id().context("no PID")?)?
        .context("wrapper exited")?;
    let execution = manager
        .agents
        .begin_execution(
            &*manager,
            workspace.id.as_str(),
            Some(identity),
            ExecutionKind::Command,
            None,
        )
        .await?;

    // The wrapper stops with its execution, so it does not count.
    let plan = manager
        .repositories
        .plan_moves(&*manager, None, Layout::Regular)
        .await?;
    assert_eq!(plan.repositories.len(), 1);
    let [listed] = plan.executions.as_slice() else {
        anyhow::bail!("expected one execution: {:?}", plan.executions);
    };
    assert_eq!(listed.id, execution.plan.id);

    assert!(
        manager
            .repositories
            .move_all(&*manager, Layout::Regular)
            .await
            .is_err()
    );
    assert!(repo.workspaces_dir.exists());

    // Another process in the directory keeps even the plan from passing.
    let mut other = tokio::process::Command::new("sleep")
        .arg("600")
        .current_dir(&workspace.path)
        .kill_on_drop(true)
        .spawn()?;
    let plan = manager
        .repositories
        .plan_moves(&*manager, None, Layout::Regular)
        .await;
    assert!(plan.is_err());
    other.kill().await?;
    wrapper.kill().await?;
    Ok(())
}

#[tokio::test]
async fn a_queued_or_running_review_holds_back_the_move() -> Result<()> {
    let (_temp, manager, repo, workspace, _root) = cloned().await?;
    // Nothing runs it here, so it stays queued.
    manager
        .store
        .run({
            let id = workspace.id.clone();
            move |db| {
                shoal_reviews::store::queue(
                    db,
                    &id,
                    &crate::reviews::ReviewTarget::Workspace,
                    &"claude".into(),
                    &Default::default(),
                    UnixTime::from_seconds(1),
                )
            }
        })
        .await?;
    assert!(
        manager
            .repositories
            .move_all(&*manager, Layout::Regular)
            .await
            .is_err()
    );
    assert!(repo.workspaces_dir.exists());
    Ok(())
}

#[tokio::test]
async fn a_waiting_ci_job_lets_the_move_go_and_a_starting_one_holds_it_back() -> Result<()> {
    let (_temp, manager, repo, _workspace, _root) = cloned().await?;
    let queue = |state: QueueState| {
        let id = repo.id.clone();
        manager.store.run(move |db| {
            db.execute(
                "INSERT INTO agent_queue(id,kind,repository_id,name,agent,state,created_at,job)
                 VALUES (?1,?2,?3,'ci-rust-4','',?4,0,'{}')",
                params![uuid::Uuid::new_v4().to_string(), QueueKind::Job, id, state],
            )?;
            Ok(())
        })
    };
    queue(QueueState::Starting).await?;
    assert!(
        manager
            .repositories
            .move_all(&*manager, Layout::Regular)
            .await
            .is_err()
    );
    assert!(repo.workspaces_dir.exists());

    manager
        .store
        .run(|db| {
            db.execute("DELETE FROM agent_queue", [])?;
            Ok(())
        })
        .await?;
    queue(QueueState::Queued).await?;
    manager
        .repositories
        .move_all(&*manager, Layout::Regular)
        .await?;
    assert!(!repo.workspaces_dir.exists());
    Ok(())
}

#[tokio::test]
async fn deleting_files_behind_a_move_deletes_them_at_the_new_place() -> Result<()> {
    let (temp, manager, repo, workspace, root) = cloned().await?;
    let file = temp.path().join("notes.md");
    fs::write(&file, "plan")?;
    manager
        .add_attached_files(
            &SpaceSelector::id(&workspace.id),
            vec![file.into()],
            &Attacher::User,
        )
        .await?;
    // A move, then a deletion, wait for the files gate this test holds.
    let held = manager.gates.lock_message_files(&repo.id).await;
    let gate = manager.gates.message_files_gate(&repo.id);
    let waiting = |count| {
        let gate = gate.clone();
        tokio::time::timeout(std::time::Duration::from_secs(60), async move {
            // This test's two handles and the guard, then each waiter.
            while Arc::strong_count(&gate) < count {
                tokio::task::yield_now().await;
            }
        })
    };
    let moving = tokio::spawn({
        let manager = manager.clone();
        async move {
            manager
                .repositories
                .move_all(&*manager, Layout::Regular)
                .await
        }
    });
    waiting(4).await.context("the move reaches the gate")?;
    let deleting = tokio::spawn({
        let (manager, id) = (manager.clone(), workspace.id.clone());
        async move {
            manager
                .remove_attached_files(&SpaceSelector::id(&id), vec![], vec![])
                .await
        }
    });
    waiting(5).await.context("the deletion reaches the gate")?;
    drop(held);
    moving.await??;
    let deleted = deleting.await??;
    let [gone] = deleted.as_slice() else {
        anyhow::bail!("expected one deleted file: {deleted:?}");
    };
    assert!(
        gone.path().unwrap().starts_with(root.join("origin")),
        "{:?}",
        gone.path().unwrap()
    );
    assert!(!gone.path().unwrap().exists());
    assert!(
        manager
            .attached_files(&SpaceSelector::id(&workspace.id))
            .await?
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn disk_cleanup_behind_a_move_checks_where_results_went() -> Result<()> {
    let (temp, manager, repo, workspace, root) = cloned().await?;
    let report = temp.path().join("report.md");
    fs::write(&report, "No leaks")?;
    manager
        .mark_space_done(
            &SpaceSelector::id(&workspace.id),
            Done {
                results: vec![report.into()],
                ..Done::default()
            },
            &Attacher::User,
        )
        .await?;
    manager
        .remove_selected(
            &SpaceSelector::id(&workspace.id),
            crate::removal::UnsavedWork::default(),
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await?;
    let [owner] =
        manager.kept_results().await?.try_into().map_err(|kept| {
            anyhow::anyhow!("expected one removed workspace with results: {kept:?}")
        })?;
    let old = repo.workspaces_dir.clone();
    // A move, then disk cleanup for the old place, wait for the gate.
    let held = manager.gates.lock_message_files(&repo.id).await;
    let gate = manager.gates.message_files_gate(&repo.id);
    let waiting = |count| {
        let gate = gate.clone();
        tokio::time::timeout(std::time::Duration::from_secs(60), async move {
            while Arc::strong_count(&gate) < count {
                tokio::task::yield_now().await;
            }
        })
    };
    let moving = tokio::spawn({
        let manager = manager.clone();
        async move {
            manager
                .repositories
                .move_all(&*manager, Layout::Regular)
                .await
        }
    });
    waiting(4).await.context("the move reaches the gate")?;
    let cleaning = tokio::spawn({
        let manager = manager.clone();
        async move {
            manager
                .delete_results_in(&owner, |path| path.starts_with(&old))
                .await
        }
    });
    waiting(5).await.context("the cleanup reaches the gate")?;
    drop(held);
    moving.await??;
    assert!(!cleaning.await??, "the results moved off the low place");
    let kept = manager
        .attached_files(&SpaceSelector::id(&workspace.id))
        .await?;
    assert!(
        kept[0].path().unwrap().starts_with(root.join("origin")),
        "{kept:?}"
    );
    assert!(kept[0].path().unwrap().exists());
    Ok(())
}

/// A manager whose repository is in `root_dir`, with the running chat
/// `master` without a repository, and that root.
async fn with_master() -> Result<(tempfile::TempDir, Arc<Manager>, crate::model::Chat, PathBuf)> {
    let (temp, manager) = test_support::indexed_manager().await;
    let origin = test_support::repository(temp.path(), "origin");
    let repo = manager
        .repositories
        .register(format!("file://{}", origin.display()).into(), None, None)
        .await?;
    let root = (repo.workspaces_dir.parent())
        .context("no root")?
        .to_owned();
    let new = crate::daemon::chats::NewChat {
        priority: None,
        narrowing: Default::default(),
        name: "master".into(),
        agent: "claude".into(),
        mode: "orchestrate".into(),
        origin: None,
        parent: None,
    };
    let chat = manager.start_new_chat(None, None, new).await?;
    let chat = manager.run_chat_agent(chat, None).await?;
    Ok((temp, manager, chat, root))
}

#[tokio::test]
async fn a_root_move_stops_the_chats_without_a_repository_and_takes_their_files() -> Result<()> {
    let (_temp, manager, chat, root) = with_master().await?;
    let tmp = chat.tmp_dir.clone().context("no temporary directory")?;
    fs::write(tmp.join("notes"), "kept")?;
    // A directory of no chat, which the move leaves behind.
    (manager.outside_tmp_layout())
        .context("no layout")?
        .prepare(&SpaceId::from("leftover"))?;

    let planned = manager
        .repositories
        .plan_moves(&*manager, None, Layout::Excluded)
        .await?;
    let stopping: Vec<_> = (planned.executions.iter())
        .map(|execution| &execution.space_id)
        .collect();
    assert_eq!(
        (planned.chats, stopping),
        (vec!["master".into()], vec![&chat.id])
    );
    assert!(
        manager
            .repositories
            .move_all(&*manager, Layout::Excluded)
            .await
            .is_err()
    );
    manager.agents.end_executions(&*manager, &chat.id).await;
    manager
        .repositories
        .move_all(&*manager, Layout::Excluded)
        .await?;

    assert_eq!(fs::read_link(&root)?, Path::new(".shoal.noindex"));
    let moved = manager.chat_by_id(&chat.id).await?.context("no chat")?;
    // The chat still runs in the configured root, now a link.
    assert_eq!(moved.workdir.root_dir(), chat.workdir.root_dir());
    assert_eq!(fs::read_to_string(tmp.join("notes"))?, "kept");
    assert!(!root.join(".tmp/leftover").exists());
    Ok(())
}

#[tokio::test]
async fn a_root_move_takes_the_files_of_removed_chats_without_a_repository() -> Result<()> {
    let (temp, manager, chat, root) = with_master().await?;
    let report = temp.path().join("report.md");
    fs::write(&report, "Plan")?;
    let selector = SpaceSelector::id(&chat.id);
    (manager.add_attached_files(&selector, vec![report.into()], &Attacher::User)).await?;
    manager.agents.end_executions(&*manager, &chat.id).await;
    manager
        .remove_selected(&selector, UnsavedWork::default(), InspectionPolicy::GitOnly)
        .await?;

    manager
        .repositories
        .move_all(&*manager, Layout::Excluded)
        .await?;

    assert_eq!(fs::read_link(&root)?, Path::new(".shoal.noindex"));
    let kept = manager.attached_files(&selector).await?;
    assert_eq!(fs::read_to_string(kept[0].path().unwrap())?, "Plan");
    Ok(())
}

#[tokio::test]
async fn a_root_move_takes_the_session_files_kept_for_chats_without_a_repository() -> Result<()> {
    let (temp, manager, chat, root) = with_master().await?;
    let selector = SpaceSelector::id(&chat.id);
    manager.agents.end_executions(&*manager, &chat.id).await;
    manager
        .remove_selected(&selector, UnsavedWork::default(), InspectionPolicy::GitOnly)
        .await?;
    let record = manager.removed_spaces().await?.remove(0);
    let contents = temp.path().join("carried");
    fs::write(&contents, "{}\n")?;
    let carried = shoal_protocol::transcripts::CarriedSessionFile {
        file: shoal_protocol::transcripts::SessionFile {
            agent: "claude".into(),
            session: "s1".into(),
            path: "project/s1.jsonl".into(),
            size: 3,
        },
        contents,
    };
    // Imported again, the chat keeps them until a caller puts them in place.
    let import = crate::daemon::restore::SpaceImport {
        repository: None,
        bundle: None,
        prerequisite: None,
        space: record,
        copy: false,
        session_files: vec![carried],
        copies: Default::default(),
        recorded: Default::default(),
        claim: crate::transfer::Claim::Plain,
    };
    manager.import_space(import).await?;

    manager
        .repositories
        .move_all(&*manager, Layout::Excluded)
        .await?;

    let kept = (manager.outside_sessions_layout(&root).existing(&chat.id))
        .context("no session files kept in the new root")?;
    assert_eq!(fs::read_to_string(kept.join("0"))?, "{}\n");
    Ok(())
}

/// Leave a directory of an import in `imports`, as one reading a package
/// does.
fn reading_import(imports: &crate::shoal_dirs::ShoalDir) -> Result<()> {
    imports.create()?;
    fs::create_dir(imports.root().join("package-1"))?;
    Ok(())
}

#[tokio::test]
async fn an_import_reading_a_package_holds_back_the_move() -> Result<()> {
    let (_temp, manager, repo, _workspace, _root) = cloned().await?;
    let imports = crate::shoal_dirs::ShoalDir::of(&repo, crate::shoal_dirs::Contents::Imports);
    reading_import(&imports)?;

    let refused = manager
        .repositories
        .move_all(&*manager, Layout::Regular)
        .await
        .unwrap_err();

    assert!(format!("{refused:#}").contains("an import"), "{refused:#}");
    assert!(repo.workspaces_dir.exists());
    Ok(())
}

#[tokio::test]
async fn a_root_move_takes_what_imports_left_in_root_dir_unless_one_reads() -> Result<()> {
    let (_temp, manager, chat, root) = with_master().await?;
    manager.agents.end_executions(&*manager, &chat.id).await;
    let imports = manager.outside_imports(&root);
    reading_import(&imports)?;
    assert!(
        manager
            .repositories
            .move_all(&*manager, Layout::Excluded)
            .await
            .is_err()
    );
    fs::remove_dir(imports.root().join("package-1"))?;

    manager
        .repositories
        .move_all(&*manager, Layout::Excluded)
        .await?;

    assert_eq!(fs::read_link(&root)?, Path::new(".shoal.noindex"));
    assert!(!root.join(".imports").exists());
    Ok(())
}

#[tokio::test]
async fn a_root_move_keeps_a_package_an_import_reads_into_the_root_it_leaves() -> Result<()> {
    let (temp, manager, chat, root) = with_master().await?;
    manager.agents.end_executions(&*manager, &chat.id).await;
    let imports = manager.outside_imports(&root);
    reading_import(&imports)?;
    let moved_to = fs::canonicalize(temp.path())?.join("new");
    let config = format!("[cleanup]\ngrace_minutes = 0\n[machine]\nroot_dir = {moved_to:?}\n");
    test_support::configure(&manager, &config).await;

    let refused = manager
        .repositories
        .move_all(&*manager, Layout::Regular)
        .await
        .unwrap_err();

    assert!(format!("{refused:#}").contains("an import"), "{refused:#}");
    assert!(imports.root().join("package-1").exists());
    Ok(())
}
