use crate::daemon::manager::Manager;
use crate::daemon::workspace::ownership::*;
use crate::git;
use crate::model::Workspace;
use anyhow::Result;
use anyhow::ensure;
use shoal_daemon_base::ownership::marked_owner;
use shoal_protocol::space_names::SpaceSelector;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

use crate::{
    daemon::test_support::{manager, repository},
    state::WorkspaceState,
};
use shoal_daemon_base::ownership::OWNER_MARKER;

fn workspace() -> Workspace {
    Workspace::new_record(
        "repository".into(),
        "workspace".into(),
        PathBuf::from("/workspace"),
        crate::model::Head::Branch("branch".into()),
        WorkspaceState::Ready,
    )
}

#[test]
fn a_marker_or_a_recorded_path_claims_an_admin_directory() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let git_dir = temp.path().join("admin");
    fs::create_dir(&git_dir)?;
    let different = temp.path().join("different");
    fs::create_dir(&different)?;
    let mut other = workspace();
    other.checkout = crate::model::Checkout::Pending {
        base: None,
        git_dir: Some(temp.path().join("admin/../admin")),
    };
    assert!(claims_git_dir(&other, &git_dir, None));
    assert!(!claims_git_dir(&other, &different, None));
    assert!(claims_git_dir(&other, &different, Some(other.id.as_str())));
    assert!(!claims_git_dir(&other, &different, Some("another")));
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
        .repositories
        .register(checkout.to_string_lossy().into_owned().into(), None, None)
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
        async move { manager.spaces.verify_worktree(&*manager, &workspace).await }
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

/// A workspace on branch `name` of a new repository whose creation, as a
/// restart cut it short, left the worktree Git created without recording or
/// marking its admin directory: failed, with its pending worktree.
async fn cut_short(name: &str) -> Result<(tempfile::TempDir, std::sync::Arc<Manager>, Workspace)> {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_string_lossy().into_owned().into(), None, None)
        .await?;
    let workspace = manager
        .create_workspace(&repo.id, name.into(), None, None, None)
        .await?;
    fs::remove_file(git::git_dir(&workspace.path).await?.join(OWNER_MARKER))?;
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                r#"UPDATE workspaces SET state=?2,git_dir=NULL,
                    pending_plan='{"source":{"kind":"new"}}' WHERE id=?1"#,
                rusqlite::params![id, WorkspaceState::Failed],
            )?;
            Ok(())
        })
        .await?;
    let workspace = manager.spaces.workspace_by_id(&workspace.id).await?;
    Ok((temp, manager, workspace))
}

#[tokio::test]
async fn a_retried_creation_records_the_admin_directory_a_cut_short_one_left() -> Result<()> {
    let (_temp, manager, workspace) = cut_short("topic").await?;
    assert!(workspace.checkout.unborn());
    // A worktree whose creation is pending was never ready to be moved.
    assert_eq!(manager.spaces.missing_worktree(&workspace).await?, None);
    let selector = SpaceSelector::id(&workspace.id);
    let created = manager
        .spaces
        .create_pending_worktree(&*manager, &selector)
        .await?;
    assert_eq!(created.state, WorkspaceState::Ready);
    let admin_dir = git::git_dir(&created.path).await?;
    assert_eq!(created.checkout.git_dir(), Some(admin_dir.as_path()));
    assert_eq!(
        marked_owner(&admin_dir)?.as_deref(),
        Some(created.id.as_str())
    );
    Ok(())
}

#[tokio::test]
async fn removal_takes_the_worktree_a_cut_short_creation_left() -> Result<()> {
    use crate::removal::{InspectionPolicy, UnsavedWork};
    let (_temp, manager, workspace) = cut_short("topic").await?;
    manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
        )
        .await?;
    assert!(!workspace.path.exists());
    assert!(
        manager
            .spaces
            .find_workspace(&workspace.id)
            .await?
            .is_none()
    );
    Ok(())
}

#[tokio::test]
async fn a_retried_creation_adds_again_what_a_cut_short_add_left() -> Result<()> {
    let (_temp, manager, workspace) = cut_short("topic").await?;
    // Git keeps an add's admin directory locked until its checkout ends.
    let repo = manager
        .repositories
        .repository(workspace.repository())
        .await?;
    crate::daemon::test_support::git(
        &repo.path,
        &["worktree", "lock", workspace.path.to_str().unwrap()],
    );
    fs::write(workspace.path.join("edited"), "by hand")?;
    let common_dir = CommonDir::of(&repo.path);
    let recovered = manager
        .spaces
        .recover_git_dir(&workspace, &common_dir)
        .await?;
    assert!(recovered.checkout.unborn());
    // Files there may be someone's work, which a retry keeps.
    let selector = SpaceSelector::id(&workspace.id);
    let refused = manager
        .spaces
        .create_pending_worktree(&*manager, &selector)
        .await;
    assert!(refused.is_err(), "{refused:?}");
    assert!(workspace.path.join("edited").exists());
    // Once only the `.git` link the add wrote is left, it adds it again.
    for entry in fs::read_dir(&workspace.path)? {
        let entry = entry?;
        match entry.file_name() == ".git" {
            true => {}
            false if entry.file_type()?.is_dir() => fs::remove_dir_all(entry.path())?,
            false => fs::remove_file(entry.path())?,
        }
    }
    let created = manager
        .spaces
        .create_pending_worktree(&*manager, &selector)
        .await?;
    assert_eq!(created.state, WorkspaceState::Ready);
    assert!(!created.path.join("edited").exists());
    let admin_dir = git::git_dir(&created.path).await?;
    assert!(!admin_dir.join("locked").exists());
    Ok(())
}
