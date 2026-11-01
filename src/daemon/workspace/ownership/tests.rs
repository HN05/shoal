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
async fn a_marker_read_that_waits_in_open_leaves_the_runtime_thread_free() -> Result<()> {
    use std::{
        ffi::CString,
        io::Write,
        os::unix::ffi::OsStrExt,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };

    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_string_lossy().into_owned().into(), None, None)
        .await?;
    let workspace = manager
        .create_workspace(&repo.id, "stalled".into(), None, None, None)
        .await?;
    // Opening a FIFO for reading waits for a writer, as opening a file waits
    // on a volume whose file access stalls.
    let marker = git::git_dir(&workspace.path).await?.join(OWNER_MARKER);
    fs::remove_file(&marker)?;
    let fifo = CString::new(marker.as_os_str().as_bytes())?;
    // SAFETY: the path is NUL-terminated and outlives the call.
    ensure!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) } == 0);
    // A read on the test's one runtime thread would keep this task from ever
    // writing the marker; the hang guard ends that read so the test fails.
    let guarded = Arc::new(AtomicBool::new(false));
    std::thread::spawn({
        let (guarded, marker) = (guarded.clone(), marker.clone());
        move || {
            std::thread::sleep(Duration::from_secs(60));
            guarded.store(true, Ordering::SeqCst);
            let _ = open_writer(&marker);
        }
    });

    let verified = tokio::spawn({
        let (manager, workspace) = (manager.clone(), workspace.clone());
        async move { manager.verify_worktree(&workspace).await }
    });
    // Opening the writer succeeds only once the read waits in open, so this
    // runs on the runtime thread while the read waits.
    let mut writer = loop {
        ensure!(
            !guarded.load(Ordering::SeqCst),
            "the marker read held the runtime thread"
        );
        match open_writer(&marker) {
            Ok(writer) => break writer,
            Err(error) if error.raw_os_error() == Some(libc::ENXIO) => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(error) => return Err(error.into()),
        }
    };
    writeln!(writer, "{}", workspace.id)?;
    drop(writer);

    verified.await??;
    Ok(())
}

/// Open the FIFO at `path` for writing, which ends a wait to open it for
/// reading; fails with `ENXIO` instead of waiting when no reader waits.
fn open_writer(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
}
