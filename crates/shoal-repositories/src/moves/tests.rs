use std::fs;

use anyhow::Result;
use shoal_core::test_support::repository;

use crate::{spotlight::Layout, stand_in::Fixture};

/// The `root_dir` the fixture's repositories move into.
fn new_root(fixture: &Fixture) -> Result<std::path::PathBuf> {
    Ok(fs::canonicalize(fixture.root.path())?.join("new"))
}

#[tokio::test]
async fn the_destination_avoids_a_deleted_workspace_recorded_there() -> Result<()> {
    let fixture = Fixture::new().await;
    let origin = repository(fixture.root.path(), "origin");
    let repo = fixture
        .register(format!("file://{}", origin.display()))
        .await;
    let root = new_root(&fixture)?;
    fixture.configure(&format!("[machine]\nroot_dir = {root:?}\n"));
    // Where the moved repository's directory would otherwise land.
    let placed = root.join("origin/placed");
    fixture
        .record_workspace(&repo, "placed", Some(placed))
        .await;
    fs::remove_dir_all(root.join("origin"))?;

    let repositories = &fixture.repositories;
    let moved = repositories
        .move_all(&fixture.daemon, Layout::Regular)
        .await?;
    let [moved] = moved.as_slice() else {
        anyhow::bail!("expected one move: {moved:?}");
    };
    assert_eq!(moved.repository.workspaces_dir, root.join("origin-2"));
    Ok(())
}

#[tokio::test]
async fn a_checkout_placed_in_its_directory_moves_with_its_source() -> Result<()> {
    let fixture = Fixture::new().await;
    let directory = fixture.root.path().join("shoal/project");
    fs::create_dir_all(&directory)?;
    let repo = fixture
        .register(repository(&directory, "checkout").display())
        .await;
    assert_eq!(repo.workspaces_dir, fs::canonicalize(&directory)?);
    let root = new_root(&fixture)?;
    fixture.configure(&format!("[machine]\nroot_dir = {root:?}\n"));
    let (repositories, daemon) = (&fixture.repositories, &fixture.daemon);

    // Another checkout registered inside the directory keeps it in place.
    let other = fixture
        .register(repository(&directory, "other").display())
        .await;
    assert!(
        repositories
            .move_all(daemon, Layout::Regular)
            .await
            .is_err()
    );
    repositories.remove(daemon, &other.id).await?;

    repositories.move_all(daemon, Layout::Regular).await?;
    let moved = repositories.repository(&repo.id).await?;
    let expected = root.join("project/checkout");
    assert_eq!(moved.path, expected);
    assert_eq!(std::path::Path::new(&moved.source), expected);
    Ok(())
}

#[tokio::test]
async fn an_excluded_move_refuses_to_link_a_recorded_workspace_path() -> Result<()> {
    let fixture = Fixture::indexed().await;
    let origin = repository(fixture.root.path(), "origin");
    let repo = fixture
        .register(format!("file://{}", origin.display()))
        .await;
    let placed = fs::canonicalize(fixture.root.path())?.join("placed");
    fixture
        .record_workspace(&repo, "placed", Some(placed.clone()))
        .await;
    fs::remove_dir_all(&placed)?;
    fixture.configure(&format!("[machine]\nroot_dir = {placed:?}\n"));
    let repositories = &fixture.repositories;
    let moved = repositories
        .move_all(&fixture.daemon, Layout::Excluded)
        .await;
    assert!(moved.is_err());
    assert!(fs::symlink_metadata(&placed).is_err());
    Ok(())
}
