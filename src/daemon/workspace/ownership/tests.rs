use super::*;
use crate::{
    daemon::workspace::identity::OWNER_MARKER,
    state::WorkspaceState,
    test_support::{manager, repository},
};

fn workspace() -> Workspace {
    Workspace::new_record(
        "repository".into(),
        "workspace".into(),
        PathBuf::from("/workspace"),
        "branch".into(),
        WorkspaceState::Ready,
    )
}

#[test]
fn ownership_uses_the_same_marker_path_and_identity_evidence() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let git_dir = temp.path().join("admin");
    fs::create_dir(&git_dir)?;
    let different = temp.path().join("different");
    fs::create_dir(&different)?;
    let mut other = workspace();
    other.git_dir = Some(temp.path().join("admin/../admin"));
    other.git_dir_id = Some("old-identity".into());

    assert!(owns_git_dir(&other, &git_dir, Some("new-identity"), None)?);
    assert!(owns_git_dir(
        &other,
        &different,
        Some("old-identity"),
        None
    )?);
    assert!(owns_git_dir(
        &other,
        Path::new("/missing"),
        None,
        Some(&other.id)
    )?);
    assert!(!owns_git_dir(
        &other,
        &different,
        Some("new-identity"),
        None
    )?);
    Ok(())
}

#[test]
fn legacy_identity_matches_the_current_birth_identity() -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    let temp = tempfile::tempdir()?;
    let git_dir = temp.path().join("admin");
    let other_dir = temp.path().join("other");
    fs::create_dir(&git_dir)?;
    fs::create_dir(&other_dir)?;
    let metadata = fs::metadata(&git_dir)?;
    let mut other = workspace();
    other.git_dir = Some(other_dir);
    other.git_dir_id = Some(format!("{}:{}", metadata.dev(), metadata.ino()));

    assert!(owns_git_dir(
        &other,
        &git_dir,
        Some(&directory_identity(&git_dir)?),
        None
    )?);
    Ok(())
}

#[tokio::test]
async fn a_marker_read_that_waits_in_open_leaves_the_runtime_to_a_time_limit() -> Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt, time::Duration};

    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_string_lossy().into_owned().into(), None, None)
        .await?;
    let workspace = manager
        .create_workspace(&repo.id, "stalled".into(), None, None, None)
        .await?;
    // Opening a FIFO waits for a writer, as opening a file waits on a volume
    // whose file access stalls.
    let marker = git::git_dir(&workspace.path).await?.join(OWNER_MARKER);
    fs::remove_file(&marker)?;
    let fifo = CString::new(marker.as_os_str().as_bytes())?;
    // SAFETY: the path is NUL-terminated and outlives the call.
    ensure!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) } == 0);
    // A read on the test's one runtime thread would keep the time limit from
    // ever ending the wait; ending it later makes that fail instead of hang.
    let stuck = marker.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(60));
        let _ = release_fifo(&stuck);
    });

    let verified =
        tokio::time::timeout(Duration::from_secs(5), manager.verify_worktree(&workspace)).await;
    assert!(verified.is_err(), "the time limit ends the wait");

    // Let the waiting read finish, so the runtime can shut down.
    release_fifo(&marker)?;
    Ok(())
}

/// Open and close the FIFO at `path` for writing, which ends a wait to open
/// it for reading, without waiting for a reader when none waits.
fn release_fifo(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map(drop)
}
