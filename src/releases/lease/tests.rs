use std::{fs, path::Path, time::SystemTime};

use super::*;
use crate::releases::sweep::tests::sweep_away;

/// The main program of a bundle of version `version` in `directory`, holding
/// `build`.
pub(in crate::releases) fn bundle(directory: &Path, version: &str, build: &str) -> PathBuf {
    let contents = directory.join("Shoal.app/Contents");
    fs::create_dir_all(contents.join("MacOS")).unwrap();
    fs::write(
        contents.join("Info.plist"),
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>CFBundleShortVersionString</key><string>{version}</string></dict></plist>"#
        ),
    )
    .unwrap();
    let executable = contents.join("MacOS/shoal");
    fs::write(&executable, build).unwrap();
    executable
}

fn copy_of(lease: &Lease) -> &Path {
    lease.executable.ancestors().nth(4).unwrap()
}

#[test]
fn a_lease_runs_a_locked_copy_of_its_build() {
    let temp = tempfile::tempdir().unwrap();
    let releases = Releases::new(temp.path().join("releases"));
    let executable = bundle(&temp.path().join("installed"), "1.2.3", "first build");

    let lease = releases.lease(&executable).unwrap();
    assert_eq!(fs::read(&lease.executable).unwrap(), b"first build");
    assert!(lease.executable.ends_with("Shoal.app/Contents/MacOS/shoal"));
    let name = copy_of(&lease).file_name().unwrap().to_str().unwrap();
    assert!(name.starts_with("1.2.3-"), "{name}");
    let contender = File::open(copy_of(&lease).join(LOCK)).unwrap();
    assert!(fs2::FileExt::try_lock_exclusive(&contender).is_err());

    let again = releases.lease(&executable).unwrap();
    assert_eq!(again.executable, lease.executable);
    fs::write(&executable, "second build").unwrap();
    let rebuilt = releases.lease(&executable).unwrap();
    assert_ne!(rebuilt.executable, lease.executable);
    assert_eq!(fs::read(&rebuilt.executable).unwrap(), b"second build");
}

#[test]
fn a_lease_copies_again_once_the_sweep_removed_its_copy() {
    let temp = tempfile::tempdir().unwrap();
    let releases = Releases::new(temp.path().join("releases"));
    let executable = bundle(&temp.path().join("installed"), "1.2.3", "build");
    let first = releases.lease(&executable).unwrap().executable;

    let copy = first.ancestors().nth(4).unwrap();
    assert_eq!(sweep_away(&releases, copy).len(), 1);

    let lease = releases.lease(&executable).unwrap();
    assert_eq!(lease.executable, first);
    assert!(releases.sweep(SystemTime::now()).unwrap().is_empty());
    assert!(lease.executable.exists());
}

#[test]
fn a_program_outside_a_bundle_has_no_copy() {
    let temp = tempfile::tempdir().unwrap();
    let releases = Releases::new(temp.path().join("releases"));
    let executable = temp.path().join("shoal");
    fs::write(&executable, "build").unwrap();
    assert!(releases.lease(&executable).is_err());
    assert!(!temp.path().join("releases").exists());
}
