use std::{
    fs::File,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use super::{FETCHING, is_as_fetched, stamp};

/// A copy a fetch wrote and stamped in a directory of its own.
fn fetched(temp: &Path) -> PathBuf {
    let directory = temp.join("7");
    std::fs::create_dir(&directory).unwrap();
    let partial = directory.join(FETCHING);
    std::fs::write(&partial, "<html>").unwrap();
    let copy = directory.join("chart.html");
    std::fs::rename(&partial, &copy).unwrap();
    stamp(&copy).unwrap();
    copy
}

#[test]
fn a_stamped_copy_reads_as_fetched() {
    let temp = tempfile::tempdir().unwrap();
    let copy = fetched(temp.path());
    assert!(is_as_fetched(copy.parent().unwrap()).unwrap());
}

#[test]
fn an_edit_in_place_moves_the_copy_from_its_stamp() {
    let temp = tempfile::tempdir().unwrap();
    let copy = fetched(temp.path());
    let later = SystemTime::now() + Duration::from_millis(1500);
    File::open(&copy).unwrap().set_modified(later).unwrap();
    assert!(!is_as_fetched(copy.parent().unwrap()).unwrap());
}

#[test]
fn a_save_that_replaces_the_copy_moves_the_directory_from_its_stamp() {
    let temp = tempfile::tempdir().unwrap();
    let copy = fetched(temp.path());
    let directory = copy.parent().unwrap();
    let stamped = std::fs::metadata(&copy).unwrap().modified().unwrap();
    // An editor writes the new contents beside the copy, keeping the copy's
    // time, and renames them over it.
    let saved = directory.join(".chart.html.swp");
    std::fs::write(&saved, "<html><body>").unwrap();
    File::open(&saved).unwrap().set_modified(stamped).unwrap();
    std::fs::rename(&saved, &copy).unwrap();
    File::open(directory)
        .unwrap()
        .set_modified(stamped + Duration::from_millis(1))
        .unwrap();
    assert!(!is_as_fetched(directory).unwrap());
}

#[test]
fn a_fetch_writing_again_beside_the_copy_is_unfinished() {
    let temp = tempfile::tempdir().unwrap();
    let copy = fetched(temp.path());
    std::fs::write(copy.with_file_name(FETCHING), "<ht").unwrap();
    assert!(!is_as_fetched(copy.parent().unwrap()).unwrap());
}
