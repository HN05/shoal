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

/// Sweep until `copy` goes, returning what that sweep removed: a process
/// another test forks holds every open lock with this process until it execs,
/// so a released lock can still be held for a moment.
pub(in crate::releases) fn sweep_away(releases: &Releases, copy: &Path) -> Vec<String> {
    for _ in 0..1000 {
        let removed = releases.sweep(SystemTime::now()).unwrap();
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

    assert!(releases.sweep(modified).unwrap().is_empty());
    assert!(creating.exists());

    releases.sweep(modified + ABANDONED).unwrap();
    assert!(!creating.exists());
}

#[test]
fn a_missing_directory_has_nothing_to_sweep() {
    let temp = tempfile::tempdir().unwrap();
    let releases = Releases::new(temp.path().join("releases"));
    assert!(releases.sweep(SystemTime::now()).unwrap().is_empty());
}
