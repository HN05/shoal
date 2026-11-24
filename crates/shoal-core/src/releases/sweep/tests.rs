use std::{fs, path::Path, time::SystemTime};

use super::*;

/// A copy named `name` in `root` with its lock file.
fn copy(root: &Path, name: &str) -> std::path::PathBuf {
    let copy = root.join(name);
    fs::create_dir_all(copy.join("Shoal.app/Contents/MacOS")).unwrap();
    fs::write(copy.join("Shoal.app/Contents/MacOS/shoal"), "binary").unwrap();
    fs::write(copy.join(LOCK), "").unwrap();
    copy
}

/// What keeps no copy.
fn nothing() -> Result<Kept> {
    Ok(Kept {
        current: Ok(None),
        running: Vec::new(),
    })
}

/// Sweep until `copy` goes, returning what that sweep removed: a process
/// another test forks holds every open lock with this process until it execs,
/// so a released lock can still be held for a moment.
pub(in crate::releases) fn sweep_away(releases: &Releases, copy: &Path) -> Vec<String> {
    for _ in 0..1000 {
        let removed = releases.sweep_keeping(SystemTime::now(), nothing).unwrap();
        if !copy.exists() {
            return removed;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("{} stayed after its lock was released", copy.display());
}

#[test]
fn a_copy_stays_while_its_lock_is_held_and_goes_once_released() {
    let temp = tempfile::tempdir().unwrap();
    let releases = Releases::new(temp.path().to_owned());
    let used = copy(temp.path(), "0.15.4-0123456789ab");
    let unused = copy(temp.path(), "0.15.3-ba9876543210");
    let lock = File::open(used.join(LOCK)).unwrap();
    fs2::FileExt::lock_shared(&lock).unwrap();

    let removed = sweep_away(&releases, &unused);
    assert_eq!(removed, ["0.15.3-ba9876543210"]);
    assert!(used.join("Shoal.app/Contents/MacOS/shoal").exists());
    assert!(!unused.exists());

    drop(lock);
    let removed = sweep_away(&releases, &used);
    assert_eq!(removed, ["0.15.4-0123456789ab"]);
    let left: Vec<_> = fs::read_dir(temp.path()).unwrap().collect();
    assert!(left.is_empty(), "the removed copies leave nothing behind");
}

#[test]
fn a_copy_being_created_stays_until_it_is_abandoned() {
    let temp = tempfile::tempdir().unwrap();
    let releases = Releases::new(temp.path().to_owned());
    let creating = temp.path().join(format!("{TEMPORARY}creating"));
    fs::create_dir_all(creating.join("Shoal.app")).unwrap();
    let modified = fs::metadata(&creating).unwrap().modified().unwrap();

    assert!(
        releases
            .sweep_keeping(modified, nothing)
            .unwrap()
            .is_empty()
    );
    assert!(creating.exists());

    releases
        .sweep_keeping(modified + ABANDONED, nothing)
        .unwrap();
    assert!(!creating.exists());
}

#[test]
fn a_missing_directory_has_nothing_to_sweep() {
    let temp = tempfile::tempdir().unwrap();
    let releases = Releases::new(temp.path().join("releases"));
    assert!(releases.sweep(SystemTime::now()).unwrap().is_empty());
}

#[test]
fn the_installed_release_s_copy_stays_unused() {
    let temp = tempfile::tempdir().unwrap();
    let releases = Releases::new(temp.path().to_owned());
    let current = copy(temp.path(), "0.20.6-82e5d4546e2d");
    let kept = || {
        Ok(Kept {
            current: Ok(Some("0.20.6-82e5d4546e2d".to_owned())),
            running: Vec::new(),
        })
    };

    assert!(
        releases
            .sweep_keeping(SystemTime::now(), kept)
            .unwrap()
            .is_empty()
    );
    assert!(current.join("Shoal.app/Contents/MacOS/shoal").exists());
}

#[test]
fn a_copy_stays_while_a_process_runs_a_program_in_it() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let releases = Releases::new(root.clone());
    let used = copy(&root, "0.20.5-0123456789ab");
    let unused = copy(&root, "0.20.4-ba9876543210");
    let kept = || {
        Ok(Kept {
            current: Ok(None),
            running: vec![used.join("Shoal.app/Contents/MacOS/shoal")],
        })
    };

    let removed = releases.sweep_keeping(SystemTime::now(), kept).unwrap();
    assert_eq!(removed, ["0.20.4-ba9876543210"]);
    assert!(used.join("Shoal.app/Contents/MacOS/shoal").exists());
    assert!(!unused.exists());
}

#[test]
fn what_keeps_a_copy_is_read_only_once_one_could_go() {
    let temp = tempfile::tempdir().unwrap();
    let releases = Releases::new(temp.path().to_owned());
    let used = copy(temp.path(), "0.20.6-82e5d4546e2d");
    let lock = File::open(used.join(LOCK)).unwrap();
    fs2::FileExt::lock_shared(&lock).unwrap();

    let removed = releases
        .sweep_keeping(SystemTime::now(), || {
            anyhow::bail!("read what keeps copies")
        })
        .unwrap();
    assert!(removed.is_empty());
}

#[test]
fn an_installed_release_that_cannot_be_named_keeps_no_copy_and_fails_the_sweep() {
    let temp = tempfile::tempdir().unwrap();
    let releases = Releases::new(temp.path().to_owned());
    let unused = copy(temp.path(), "0.20.5-0123456789ab");
    let kept = || {
        Ok(Kept {
            current: Err(io::Error::other("no installed release")),
            running: Vec::new(),
        })
    };

    let error = releases.sweep_keeping(SystemTime::now(), kept).unwrap_err();
    assert!(
        error.to_string().contains("no installed release"),
        "{error:#}"
    );
    assert!(!unused.exists());
}

#[test]
fn copies_stay_while_what_keeps_them_is_unknown() {
    let temp = tempfile::tempdir().unwrap();
    let releases = Releases::new(temp.path().to_owned());
    let unused = copy(temp.path(), "0.20.5-0123456789ab");

    let error = releases
        .sweep_keeping(SystemTime::now(), || anyhow::bail!("list processes"))
        .unwrap_err();
    assert!(error.to_string().contains("list processes"), "{error:#}");
    assert!(unused.exists());
}
