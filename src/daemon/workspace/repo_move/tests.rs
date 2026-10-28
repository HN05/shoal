use super::*;
use crate::{
    daemon::workspace::{ExecutionKind, identity},
    test_support,
};
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
        .register_repository(format!("file://{}", origin.display()).into(), None, None)
        .await?;
    let workspace = manager
        .create_workspace(&repo.id, "feature".into(), None, None, None)
        .await?;
    let root = fs::canonicalize(temp.path())?.join("new");
    test_support::configure(&manager, &format!("root_dir = {:?}\n", root)).await;
    Ok((temp, manager, repo, workspace, root))
}

#[tokio::test]
async fn moves_the_clone_and_workspaces_into_the_new_root() -> Result<()> {
    let (_temp, manager, repo, workspace, root) = cloned().await?;
    let old = repo.workspaces_dir.clone().context("no directory")?;
    // The seed's locked worktree moves and reconnects too.
    test_support::git(
        &repo.path,
        &["worktree", "add", "--lock", "--detach", "../.seed/worktree"],
    );

    let moved = manager.move_repositories(Layout::Regular).await?;
    let [moved] = moved.as_slice() else {
        anyhow::bail!("expected one move: {moved:?}");
    };
    let directory = root.join("origin");
    assert_eq!(moved.from, old);
    assert_eq!(moved.workspace_paths, std::slice::from_ref(&workspace.path));
    assert_eq!(moved.repository.workspaces_dir.as_ref(), Some(&directory));
    assert_eq!(moved.repository.path, directory.join(".checkout"));
    assert!(!old.exists());

    let workspace = manager.workspace(&workspace.id).await?;
    assert_eq!(workspace.path, directory.join("feature"));
    assert_eq!(workspace.state, WorkspaceState::Ready);
    assert!(
        workspace
            .git_dir
            .as_ref()
            .is_some_and(|dir| dir.starts_with(&directory))
    );
    manager.verify_worktree(&workspace).await?;
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
    assert!(manager.move_repositories(Layout::Regular).await?.is_empty());
    let next = manager
        .create_workspace(&repo.id, "next".into(), None, None, None)
        .await?;
    assert_eq!(next.path, directory.join("next"));
    Ok(())
}

#[tokio::test]
async fn an_interrupted_rename_finishes_and_an_incomplete_copy_rolls_back() -> Result<()> {
    let (_temp, manager, repo, workspace, root) = cloned().await?;
    let source = repo.workspaces_dir.clone().context("no directory")?;
    let destination = root.join("origin");
    fs::create_dir_all(&destination)?;

    // A copy cut short leaves the source, so the move rolls back.
    manager.record_move(&repo.id, &source, &destination).await?;
    let error = manager
        .reserve_lifecycle(&workspace.id, WorkspaceState::Removing)
        .await
        .err()
        .context("removal reserved during a move")?;
    assert!(error.to_string().contains("repository move is incomplete"));
    let error = manager
        .start_chat(
            repo.id.clone().into(),
            "chat-1".into(),
            "codex".into(),
            std::process::id(),
        )
        .await
        .err()
        .context("chat started during a move")?;
    assert!(error.to_string().contains("repository move is incomplete"));
    let error = manager
        .remove_repository(&repo.id)
        .await
        .err()
        .context("removed a repository during its move")?;
    assert!(format!("{error:#}").contains("repository move is incomplete"));
    // Quarantine fails the reserved workspaces; setup must not revive one.
    manager.store.quarantine_interrupted_operations().await?;
    test_support::configure(
        &manager,
        &format!("root_dir = {root:?}\npre_setup_cmd = \"true\"\n"),
    )
    .await;
    let error = manager
        .begin_execution(&workspace.id, None, ExecutionKind::Setup, None)
        .await
        .err()
        .context("setup started during a move")?;
    assert!(
        format!("{error:#}").contains("repository move is incomplete"),
        "{error:#}"
    );
    fs::write(destination.join("partial"), "")?;
    manager.finish_repository_moves().await?;
    assert!(!destination.exists());
    assert_eq!(manager.repository(&repo.id).await?.path, repo.path);
    assert_eq!(
        manager.workspace(&workspace.id).await?.state,
        WorkspaceState::Ready
    );

    // A completed rename finishes when the daemon starts again, leaving a
    // directory created at the old path since.
    fs::create_dir_all(&destination)?;
    manager.record_move(&repo.id, &source, &destination).await?;
    fs::rename(&source, &destination)?;
    fs::create_dir(&source)?;
    fs::write(source.join("unrelated"), "keep")?;
    manager.store.quarantine_interrupted_operations().await?;
    manager.finish_repository_moves().await?;
    let workspace = manager.workspace(&workspace.id).await?;
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
    let source = repo.workspaces_dir.clone().context("no directory")?;
    let destination = root.join("origin");
    fs::create_dir_all(&destination)?;

    // What a move to another filesystem leaves before the original goes.
    manager.record_move(&repo.id, &source, &destination).await?;
    fsutil::copy_directory(&source, &destination).await?;
    manager.switch_move(&repo.id, &source, &destination).await?;
    // An unavailable copy keeps the original.
    let away = root.join("unmounted");
    fs::rename(&destination, &away)?;
    assert!(manager.finish_repository_moves().await.is_err());
    assert!(source.join(".checkout").exists());
    fs::rename(&away, &destination)?;
    manager.finish_repository_moves().await?;

    assert!(!source.exists());
    let workspace = manager.workspace(&workspace.id).await?;
    assert_eq!(workspace.path, destination.join("feature"));
    assert!(
        workspace
            .git_dir
            .as_ref()
            .is_some_and(|dir| dir.starts_with(&destination))
    );
    manager.verify_worktree(&workspace).await?;
    assert_eq!(
        git::head_branch(&workspace.path, false, git::run)
            .await?
            .as_deref(),
        Some("feature")
    );
    let checkout = manager.repository(&repo.id).await?.path;
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
    let source = repo.workspaces_dir.clone().context("no directory")?;
    let destination = root.join("origin");
    fs::create_dir_all(&destination)?;

    // The move holds the repository's Git gate from its check to its end.
    let gate = manager.gates.lock_git(&repo.id).await;
    let checkpointing = manager.clone();
    let id = workspace.id.clone();
    let mut checkpoint = Box::pin(async move { checkpointing.take_checkpoint(&id, None).await });
    test_support::assert_pending(&mut checkpoint).await;
    manager.record_move(&repo.id, &source, &destination).await?;
    drop(gate);
    let error = checkpoint
        .await
        .err()
        .context("checkpoint taken during a move")?;
    assert!(error.to_string().contains("repository move is incomplete"));

    manager.finish_repository_moves().await?;
    manager.take_checkpoint(&workspace.id, None).await?;
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

    let moved = manager.move_repositories(Layout::Regular).await?;
    let [moved] = moved.as_slice() else {
        anyhow::bail!("expected one move: {moved:?}");
    };
    assert_eq!(
        moved.repository.workspaces_dir.as_deref(),
        Some(root.join("origin-2").as_path())
    );
    Ok(())
}

#[test]
fn the_destination_avoids_a_missing_checkout_recorded_inside_it() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = fs::canonicalize(temp.path())?;
    let clone = Repository {
        id: "clone".into(),
        path: root.join("project/.checkout"),
        source: "https://example.invalid/clone.git".into(),
        last_used: 1,
        name: None,
        workspaces_dir: Some(root.join("elsewhere")),
        contribution: false,
    };
    let reserved =
        registry::reserve_directory(&root, "project", std::slice::from_ref(&clone), &[])?;
    assert_eq!(reserved, root.join("project-2"));

    // A root inside a recorded directory has no free name at all.
    let nested = root.join("elsewhere/nested");
    let error = registry::reserve_directory(&nested, "project", &[clone], &[])
        .err()
        .context("reserved inside a recorded directory")?;
    assert!(error.to_string().contains("already records"), "{error:#}");
    Ok(())
}

#[tokio::test]
async fn a_move_destination_and_workspace_paths_exclude_each_other() -> Result<()> {
    let (temp, manager, repo, _workspace, root) = cloned().await?;
    let source = repo.workspaces_dir.clone().context("no directory")?;

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
    let error = manager
        .record_move(&repo.id, &source, &taken)
        .await
        .err()
        .context("recorded a move over a workspace")?;
    assert!(format!("{error:#}").contains("placed where the repository directory"));

    // While a move is recorded, no repository places a workspace there.
    let destination = root.join("origin-2");
    fs::create_dir_all(&destination)?;
    manager.record_move(&repo.id, &source, &destination).await?;
    let other = test_support::repository(temp.path(), "other");
    let other = manager
        .register_repository(other.to_str().context("path")?.into(), None, None)
        .await?;
    let error = manager
        .create_workspace(
            &other.id,
            "intruder".into(),
            None,
            None,
            Some(destination.join("intruder")),
        )
        .await
        .err()
        .context("placed a workspace in a move destination")?;
    assert!(format!("{error:#}").contains("where a repository directory is moving"));
    manager.finish_repository_moves().await?;
    Ok(())
}

#[tokio::test]
async fn a_replaced_worktree_keeps_the_move_unfinished() -> Result<()> {
    let (_temp, manager, repo, workspace, root) = cloned().await?;
    let source = repo.workspaces_dir.clone().context("no directory")?;
    let destination = root.join("origin");
    fs::create_dir_all(&destination)?;
    manager.record_move(&repo.id, &source, &destination).await?;
    fs::rename(&source, &destination)?;
    let admin = destination.join(".checkout/.git/worktrees/feature");
    identity::mark_owner(&admin, "another")?;

    let error = manager
        .finish_repository_moves()
        .await
        .err()
        .context("adopted a replaced worktree")?;
    assert!(format!("{error:#}").contains("another Shoal workspace"));
    assert!(manager.ensure_repository_available(&repo.id).await.is_err());
    assert_eq!(
        manager.workspace(&workspace.id).await?.state,
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
        .register_repository(checkout.to_str().context("path")?.into(), None, None)
        .await?;
    assert_eq!(
        repo.workspaces_dir.as_deref(),
        Some(fs::canonicalize(&directory)?.as_path())
    );
    let root = fs::canonicalize(temp.path())?.join("new");
    test_support::configure(&manager, &format!("root_dir = {:?}\n", root)).await;

    // Another checkout registered inside the directory keeps it in place.
    let other = test_support::repository(&directory, "other");
    let other = manager
        .register_repository(other.to_str().context("path")?.into(), None, None)
        .await?;
    let error = manager
        .move_repositories(Layout::Regular)
        .await
        .err()
        .context("moved another repository's checkout")?;
    assert!(format!("{error:#}").contains("belongs to the repository"));
    manager.remove_repository(&other.id).await?;

    manager.move_repositories(Layout::Regular).await?;
    let moved = manager.repository(&repo.id).await?;
    let expected = root.join("project/checkout");
    assert_eq!(moved.path, expected);
    assert_eq!(Path::new(&moved.source), expected);
    Ok(())
}

#[tokio::test]
async fn refuses_a_root_inside_a_repository_directory_or_workspace() -> Result<()> {
    let (temp, manager, repo, _workspace, _root) = cloned().await?;
    let placed = fs::canonicalize(temp.path())?.join("placed");
    manager
        .create_workspace(&repo.id, "placed".into(), None, None, Some(placed.clone()))
        .await?;
    for (inside, message) in [
        (
            repo.workspaces_dir
                .clone()
                .context("no directory")?
                .join("nested"),
            "inside the repository directory",
        ),
        (placed.join("nested"), "inside the workspace placed"),
    ] {
        test_support::configure(&manager, &format!("root_dir = {:?}\n", inside)).await;
        let error = manager
            .move_repositories(Layout::Regular)
            .await
            .err()
            .context("moved into itself")?;
        assert!(error.to_string().contains(message), "{error:#}");
    }
    Ok(())
}

#[tokio::test]
async fn an_excluded_move_empties_the_indexed_root_and_links_it() -> Result<()> {
    let (temp, manager) = test_support::indexed_manager().await;
    let origin = test_support::repository(temp.path(), "origin");
    let repo = manager
        .register_repository(format!("file://{}", origin.display()).into(), None, None)
        .await?;
    let workspace = manager
        .create_workspace(&repo.id, "feature".into(), None, None, None)
        .await?;
    let old = repo.workspaces_dir.clone().context("no directory")?;
    let root = old.parent().context("no root")?.to_owned();
    let excluded = root.with_file_name(".shoal.noindex");

    let plan = manager
        .plan_repository_moves(None, Layout::Excluded)
        .await?;
    assert_eq!(plan.directory, excluded);
    assert!(fs::symlink_metadata(&root)?.is_dir());
    fs::write(root.join("notes"), "")?;
    let error = manager
        .move_repositories(Layout::Excluded)
        .await
        .err()
        .context("moved with a file left in the root")?;
    assert!(error.to_string().contains("--regular-dir"), "{error:#}");
    assert!(old.exists());

    fs::remove_file(root.join("notes"))?;
    let moved = manager.move_repositories(Layout::Excluded).await?;
    let [moved] = moved.as_slice() else {
        anyhow::bail!("expected one move: {moved:?}");
    };
    let directory = excluded.join("origin");
    assert_eq!(moved.repository.workspaces_dir.as_ref(), Some(&directory));
    assert_eq!(fs::read_link(&root)?, Path::new(".shoal.noindex"));
    let workspace = manager.workspace(&workspace.id).await?;
    assert_eq!(workspace.path, directory.join("feature"));
    manager.verify_worktree(&workspace).await?;

    // A regular directory replaces the link and takes the repository back.
    let moved = manager.move_repositories(Layout::Regular).await?;
    let [moved] = moved.as_slice() else {
        anyhow::bail!("expected one move: {moved:?}");
    };
    assert_eq!(
        moved.repository.workspaces_dir.as_ref(),
        Some(&root.join("origin"))
    );
    assert!(fs::symlink_metadata(&root)?.is_dir());
    Ok(())
}

#[tokio::test]
async fn an_excluded_move_refuses_to_link_a_recorded_workspace_path() -> Result<()> {
    let (temp, manager) = test_support::indexed_manager().await;
    let origin = test_support::repository(temp.path(), "origin");
    let repo = manager
        .register_repository(format!("file://{}", origin.display()).into(), None, None)
        .await?;
    let placed = fs::canonicalize(temp.path())?.join("placed");
    manager
        .create_workspace(&repo.id, "placed".into(), None, None, Some(placed.clone()))
        .await?;
    fs::remove_dir_all(&placed)?;
    test_support::configure(&manager, &format!("root_dir = {:?}\n", placed)).await;
    let error = manager
        .move_repositories(Layout::Excluded)
        .await
        .err()
        .context("linked a workspace path")?;
    assert!(
        error.to_string().contains("inside the workspace placed"),
        "{error:#}"
    );
    assert!(fs::symlink_metadata(&placed).is_err());
    Ok(())
}
