use super::*;
use crate::test_support::native_cloning;

fn set_time(path: &Path, seconds: u64) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_secs(seconds))
        .unwrap();
}

fn time(path: &Path) -> u64 {
    fs::metadata(path)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// How these tests copy output trees: Shoal's clone where the filesystem
/// clones natively, otherwise a plain copy that keeps times, so snapshot
/// application and rollback run on every filesystem.
fn copier(root: &Path) -> impl AsyncFn(&Path, &Path) -> Result<()> {
    let native = native_cloning(root);
    async move |source: &Path, destination: &Path| {
        if native {
            clone_tree(source, destination).await
        } else {
            copy(source, destination).await
        }
    }
}

async fn copy(source: &Path, destination: &Path) -> Result<()> {
    let status = tokio::process::Command::new("cp")
        .args(["-pPR", "--"])
        .arg(source)
        .arg(destination)
        .status()
        .await?;
    anyhow::ensure!(status.success(), "copy {} failed", source.display());
    Ok(())
}

/// Stage, install and retire a snapshot as a seed build does.
async fn freeze(
    copy: &impl AsyncFn(&Path, &Path) -> Result<()>,
    layout: &Layout,
    commit: &str,
    paths: &[String],
    files: Vec<String>,
) -> Result<()> {
    let staged = Snapshot::stage_with(copy, layout, paths).await?;
    staged.install(layout, commit.into(), "s".into(), files)?;
    layout.remove_retired()
}

#[tokio::test]
async fn snapshots_clone_natively_or_not_at_all() {
    let root = tempfile::tempdir().unwrap();
    let layout = Layout::new(root.path(), &"repo".into());
    layout.claim().unwrap();
    fs::create_dir_all(layout.worktree().join("out")).unwrap();
    fs::write(layout.worktree().join("out/built"), "built").unwrap();
    let paths = vec!["out".to_owned()];
    let frozen = freeze(&clone_tree, &layout, "c1", &paths, Vec::new()).await;
    if !native_cloning(root.path()) {
        // Copying would cost the disk space cloning saves.
        assert!(frozen.is_err());
        assert!(Snapshot::open(&layout).unwrap().is_none());
        return;
    }
    frozen.unwrap();
    let workspace = root.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    let snapshot = Snapshot::open(&layout).unwrap().unwrap();
    let cloned = snapshot
        .clone_outputs(&layout, &workspace, &paths)
        .await
        .unwrap();
    assert_eq!(cloned, ["out"]);
    assert_eq!(
        fs::read_to_string(workspace.join("out/built")).unwrap(),
        "built"
    );
}

#[tokio::test]
async fn new_worktrees_get_the_outputs_and_the_times_of_unchanged_files() {
    let root = tempfile::tempdir().unwrap();
    let layout = Layout::new(root.path(), &"repo".into());
    layout.claim().unwrap();
    let seed = layout.worktree();
    fs::create_dir_all(seed.join("src")).unwrap();
    fs::create_dir_all(seed.join("target/debug")).unwrap();
    for file in ["src/same.rs", "src/changed.rs", "src/modified.rs"] {
        fs::write(seed.join(file), "seed").unwrap();
        set_time(&seed.join(file), 1_000);
    }
    fs::write(seed.join("target/debug/app"), "binary").unwrap();
    set_time(&seed.join("target/debug/app"), 2_000);
    let copy = copier(root.path());
    // The build modified one tracked file, so the caller leaves it out.
    let files = vec!["src/same.rs".into(), "src/changed.rs".into()];
    let paths = vec!["target".to_owned(), "missing".to_owned()];
    freeze(&copy, &layout, "c1", &paths, files).await.unwrap();
    // A rebuild in the seed worktree does not reach the snapshot.
    fs::write(seed.join("target/debug/app"), "rebuilding").unwrap();

    let workspace = root.path().join("workspace");
    fs::create_dir_all(workspace.join("src")).unwrap();
    for file in ["src/same.rs", "src/changed.rs", "src/modified.rs"] {
        fs::write(workspace.join(file), "checkout").unwrap();
        set_time(&workspace.join(file), 5_000);
    }
    // Read-only tracked files still take the seed's time.
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(
        workspace.join("src/same.rs"),
        fs::Permissions::from_mode(0o444),
    )
    .unwrap();
    let snapshot = Snapshot::open(&layout).unwrap().unwrap();
    assert_eq!(snapshot.commit, "c1");
    let cloned = snapshot
        .clone_outputs_with(&copy, &layout, &workspace, &paths)
        .await
        .unwrap();
    assert_eq!(cloned, ["target"]);
    assert_eq!(
        fs::read_to_string(workspace.join("target/debug/app")).unwrap(),
        "binary"
    );
    assert_eq!(time(&workspace.join("target/debug/app")), 2_000);
    let changed = HashSet::from(["src/changed.rs".to_owned()]);
    snapshot.restore_times(&workspace, &changed).unwrap();
    assert_eq!(time(&workspace.join("src/same.rs")), 1_000);
    assert_eq!(time(&workspace.join("src/changed.rs")), 5_000);
    assert_eq!(time(&workspace.join("src/modified.rs")), 5_000);
    // Existing outputs are never replaced.
    assert!(
        snapshot
            .clone_outputs_with(&copy, &layout, &workspace, &paths)
            .await
            .unwrap()
            .is_empty()
    );

    freeze(&copy, &layout, "c2", &paths, Vec::new())
        .await
        .unwrap();
    let snapshot = Snapshot::open(&layout).unwrap().unwrap();
    assert_eq!(snapshot.commit, "c2");
    assert!(!layout.staging().exists() && !layout.retired().exists());
}

#[test]
fn no_snapshot_before_the_first_build() {
    let root = tempfile::tempdir().unwrap();
    assert!(
        Snapshot::open(&Layout::new(root.path(), &"repo".into()))
            .unwrap()
            .is_none()
    );
}

#[test]
fn removal_keeps_what_shoal_did_not_put_beside_the_seed() {
    let root = tempfile::tempdir().unwrap();
    let layout = Layout::new(root.path(), &"repo".into());
    layout.claim().unwrap();
    fs::create_dir_all(layout.snapshot().join(OUTPUTS)).unwrap();
    fs::write(layout.log(), "log").unwrap();
    fs::write(layout.root().join("notes"), "mine").unwrap();
    layout.remove_owned().unwrap();
    assert!(!layout.snapshot().exists() && !layout.log().exists());
    assert!(layout.root().join("notes").exists());
    // What is left is no longer Shoal's.
    assert!(layout.ensure_owned().is_err());
    fs::remove_file(layout.root().join("notes")).unwrap();
    fs::remove_dir(layout.root()).unwrap();
    layout.claim().unwrap();
    layout.remove_owned().unwrap();
    assert!(!layout.root().exists());
    layout.remove_owned().unwrap();
}

#[test]
fn a_symlinked_seed_directory_is_left_alone() {
    let root = tempfile::tempdir().unwrap();
    let elsewhere = root.path().join("elsewhere");
    fs::create_dir_all(elsewhere.join("snapshot")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, root.path().join(".seed")).unwrap();
    let layout = Layout::new(root.path(), &"repo".into());
    assert!(layout.remove_owned().is_err());
    assert!(layout.claim().is_err());
    assert!(elsewhere.join("snapshot").exists());
    // Another repository's seed directory is not this one's either.
    fs::remove_file(root.path().join(".seed")).unwrap();
    Layout::new(root.path(), &"other".into()).claim().unwrap();
    assert!(layout.claim().is_err() && layout.remove_owned().is_err());
}

#[tokio::test]
async fn a_failed_clone_leaves_no_outputs_behind() {
    let root = tempfile::tempdir().unwrap();
    let copy = copier(root.path());
    let layout = Layout::new(root.path(), &"repo".into());
    layout.claim().unwrap();
    for path in ["first", "second"] {
        fs::create_dir_all(layout.worktree().join(path)).unwrap();
        fs::write(layout.worktree().join(path).join("built"), path).unwrap();
    }
    let paths = vec!["first".to_owned(), "second".to_owned()];
    freeze(&copy, &layout, "c1", &paths, Vec::new())
        .await
        .unwrap();
    // The second output disappears from the snapshot, so cloning it fails.
    fs::remove_dir_all(layout.snapshot().join(OUTPUTS).join("second")).unwrap();
    let workspace = root.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    let snapshot = Snapshot::open(&layout).unwrap().unwrap();
    assert!(
        snapshot
            .clone_outputs_with(&copy, &layout, &workspace, &paths)
            .await
            .is_err()
    );
    assert!(!workspace.join("first").exists() && !workspace.join("second").exists());
}

#[tokio::test]
async fn outputs_never_pass_through_a_symlink() {
    let root = tempfile::tempdir().unwrap();
    let copy = copier(root.path());
    let layout = Layout::new(root.path(), &"repo".into());
    layout.claim().unwrap();
    fs::create_dir_all(layout.worktree().join("ios/build")).unwrap();
    let paths = vec!["ios/build".to_owned()];
    freeze(&copy, &layout, "c1", &paths, Vec::new())
        .await
        .unwrap();
    let workspace = root.path().join("workspace");
    let outside = root.path().join("outside");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, workspace.join("ios")).unwrap();
    let snapshot = Snapshot::open(&layout).unwrap().unwrap();
    assert!(
        snapshot
            .clone_outputs_with(&copy, &layout, &workspace, &paths)
            .await
            .is_err()
    );
    // Nor does the spare, which stays for another worktree.
    snapshot.refill_spare_with(&copy, &layout).await.unwrap();
    let taken = snapshot.take_spare(&layout, &workspace, &paths).unwrap();
    assert!(taken.is_none() && layout.has_spare());
    assert!(!outside.join("build").exists());
}

#[tokio::test]
async fn a_new_worktree_takes_the_spare_instead_of_cloning() {
    let root = tempfile::tempdir().unwrap();
    let copy = copier(root.path());
    let layout = Layout::new(root.path(), &"repo".into());
    layout.claim().unwrap();
    fs::create_dir_all(layout.worktree().join("ios/build")).unwrap();
    fs::write(layout.worktree().join("ios/build/app"), "binary").unwrap();
    set_time(&layout.worktree().join("ios/build/app"), 2_000);
    let paths = vec!["ios/build".to_owned()];
    freeze(&copy, &layout, "c1", &paths, Vec::new())
        .await
        .unwrap();
    let snapshot = Snapshot::open(&layout).unwrap().unwrap();
    assert!(snapshot.refill_spare_with(&copy, &layout).await.unwrap());
    assert!(!snapshot.refill_spare_with(&copy, &layout).await.unwrap());

    // A worktree that has an output already clones the rest.
    let built = root.path().join("built");
    fs::create_dir_all(built.join("ios/build")).unwrap();
    let taken = snapshot.take_spare(&layout, &built, &paths).unwrap();
    assert!(taken.is_none() && layout.has_spare());

    let workspace = root.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    let taken = snapshot.take_spare(&layout, &workspace, &paths).unwrap();
    assert_eq!(taken.as_deref(), Some(paths.as_slice()));
    let app = workspace.join("ios/build/app");
    assert_eq!(fs::read_to_string(&app).unwrap(), "binary");
    assert_eq!(time(&app), 2_000);
    // The snapshot keeps its outputs for clones and later spares.
    assert!(!layout.has_spare() && !layout.spare_taken().exists());
    let next = root.path().join("next");
    fs::create_dir_all(&next).unwrap();
    assert!(
        snapshot
            .take_spare(&layout, &next, &paths)
            .unwrap()
            .is_none()
    );
    assert!(snapshot.refill_spare_with(&copy, &layout).await.unwrap());

    // A new snapshot retires the spare of the one it replaces.
    freeze(&copy, &layout, "c2", &paths, Vec::new())
        .await
        .unwrap();
    assert!(!layout.has_spare());
}
