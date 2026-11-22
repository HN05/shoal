use anyhow::{Context, Result};
use shoal_core::test_support::{git, repository};
use shoal_daemon_base::ownership::paths::real_directory_identity;
use std::fs;

use super::verify_directory;
use crate::stand_in::Fixture;

#[tokio::test]
async fn removal_refuses_a_checkout_whose_config_sets_bare() -> Result<()> {
    let fixture = Fixture::new().await;
    let (root, repositories) = (&fixture.root, &fixture.repositories);
    let checkout = repository(root.path(), "repo");
    let repo = repositories
        .register(checkout.to_string_lossy().into_owned().into(), None, None)
        .await?;
    git(&repo.path, &["config", "core.bare", "true"]);

    assert!(
        repositories
            .remove(&fixture.daemon, &repo.id)
            .await
            .is_err()
    );
    assert!(repo.path.exists());

    git(&repo.path, &["config", "core.bare", "false"]);
    repositories.remove(&fixture.daemon, &repo.id).await?;
    assert!(!repo.path.exists());
    Ok(())
}

#[test]
fn recorded_identity_verifies_and_refuses_a_replaced_directory() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = fs::canonicalize(temp.path())?;
    let directory = root.join("checkout");
    fs::create_dir(&directory)?;
    let current = real_directory_identity(&directory)?.context("missing identity")?;
    verify_directory(&directory, Some(&current), false)?;
    assert!(verify_directory(&directory, None, true).is_err());

    let moved = root.join("moved");
    fs::rename(&directory, &moved)?;
    fs::create_dir(&directory)?;
    assert!(verify_directory(&directory, Some(&current), true).is_err());
    fs::remove_dir(&directory)?;
    verify_directory(&directory, Some(&current), true)?;
    verify_directory(&directory, None, false)?;
    assert!(verify_directory(&directory, Some(&current), false).is_err());
    Ok(())
}

#[tokio::test]
async fn removal_refuses_a_checkout_holding_another_repositorys_directory() {
    let fixture = Fixture::new().await;
    let (root, repositories) = (&fixture.root, &fixture.repositories);
    let checkout = repository(root.path(), "project");
    let elsewhere = repository(root.path(), "elsewhere");
    let other = repositories
        .register(elsewhere.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    // An earlier root_dir placed the other repository's directory here.
    let reserved = std::fs::canonicalize(&checkout)
        .unwrap()
        .join(".shoal")
        .join("elsewhere");
    std::fs::create_dir_all(reserved.join(".shoal")).unwrap();
    let (id, directory) = (other.id.clone(), reserved.clone());
    repositories
        .store
        .run(move |db| {
            db.execute(
                "UPDATE repositories SET workspaces_dir=?2 WHERE id=?1",
                rusqlite::params![id, directory.to_str()],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let project = repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    assert!(
        repositories
            .remove(&fixture.daemon, &project.id)
            .await
            .is_err()
    );
    assert!(reserved.join(".shoal").exists());
}
