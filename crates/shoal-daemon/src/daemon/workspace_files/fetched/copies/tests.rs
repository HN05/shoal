use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use super::remove_written_before;
use crate::shoal_dirs::{Contents, ShoalDir};

/// A day ago, before which copies go in these tests.
fn cutoff() -> SystemTime {
    SystemTime::now() - Duration::from_secs(24 * 3600)
}

/// Write a copy at `space/file`, last written at `at`.
fn copy(dir: &ShoalDir, space: &str, file: &str, at: SystemTime) -> PathBuf {
    let directory = dir.root().join(space).join(file);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("chart.html");
    std::fs::write(&path, "<html>").unwrap();
    touch(&path, at);
    touch(&directory, at);
    path
}

fn touch(path: &Path, at: SystemTime) {
    std::fs::File::open(path).unwrap().set_modified(at).unwrap();
}

fn marked(temp: &Path) -> ShoalDir {
    let dir = ShoalDir::outside_repositories(temp, Path::new("/state"), Contents::Fetched);
    dir.create().unwrap();
    dir
}

#[test]
fn copies_fetched_before_the_cutoff_go_with_the_spaces_they_leave_empty() {
    let temp = tempfile::tempdir().unwrap();
    let dir = marked(temp.path());
    let long_ago = cutoff() - Duration::from_secs(3600);
    let old = copy(&dir, "space-a", "1", long_ago);
    let recent = copy(&dir, "space-b", "2", SystemTime::now());
    let beside = copy(&dir, "space-b", "3", long_ago);

    assert!(remove_written_before(&dir, cutoff()).unwrap());

    assert!(!old.parent().unwrap().parent().unwrap().exists());
    assert!(recent.exists());
    assert!(!beside.parent().unwrap().exists());
    assert!(
        dir.is_own(),
        "the directory keeps its mark for later fetches"
    );
}

#[test]
fn a_copy_edited_since_its_fetch_stays() {
    let temp = tempfile::tempdir().unwrap();
    let dir = marked(temp.path());
    let edited = copy(&dir, "space", "1", cutoff() - Duration::from_secs(3600));
    touch(&edited, SystemTime::now());

    assert!(!remove_written_before(&dir, cutoff()).unwrap());
    assert!(edited.exists());
}

#[test]
fn a_directory_shoal_did_not_create_stays() {
    let temp = tempfile::tempdir().unwrap();
    let dir = ShoalDir::outside_repositories(temp.path(), Path::new("/state"), Contents::Fetched);
    std::fs::create_dir(dir.root()).unwrap();
    let theirs = copy(&dir, "space", "1", cutoff() - Duration::from_secs(3600));

    assert!(!remove_written_before(&dir, SystemTime::now()).unwrap());
    assert!(theirs.exists());
}
