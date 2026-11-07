use std::fs;

use super::*;
use crate::{env, fsutil::FileIdentity};

fn variable<'a>(replacement: &'a Replacement, name: &str) -> Option<&'a OsString> {
    replacement
        .variables
        .iter()
        .find_map(|(variable, value)| (*variable == name).then_some(value))
}

fn inherited(fd: RawFd) -> bool {
    // SAFETY: F_GETFD only reads the descriptor's flags.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    flags >= 0 && flags & libc::FD_CLOEXEC == 0
}

#[test]
fn a_replacement_runs_the_copy_and_hands_it_the_lock_and_the_invoked_path() {
    let temp = tempfile::tempdir().unwrap();
    let releases = Releases::new(temp.path().join("releases"));
    let executable = lease::tests::bundle(&temp.path().join("installed"), "1.2.3", "build");
    let invoked = Invoked::at(executable.clone());

    let replacement = Replacement::of(Some(&releases), executable.clone(), &invoked).unwrap();

    assert!(
        replacement
            .program
            .starts_with(temp.path().join("releases"))
    );
    assert_eq!(
        variable(&replacement, env::RELEASE_INVOKED),
        Some(&executable.clone().into_os_string())
    );
    let identity = variable(&replacement, env::RELEASE_INVOKED_IDENTITY).unwrap();
    let identity: FileIdentity = serde_json::from_str(identity.to_str().unwrap()).unwrap();
    assert_eq!(Some(identity), invoked.identity);
    let locks = variable(&replacement, env::RELEASE_LOCKS).unwrap();
    let lock: RawFd = locks.to_str().unwrap().parse().unwrap();
    assert!(inherited(lock), "the lock stays open across the exec");
}

#[test]
fn a_replacement_runs_the_release_itself_when_it_cannot_be_copied() {
    let temp = tempfile::tempdir().unwrap();
    let blocked = temp.path().join("state");
    fs::write(&blocked, "not a directory").unwrap();
    let releases = Releases::new(blocked.join("releases"));
    let executable = lease::tests::bundle(&temp.path().join("installed"), "1.2.3", "build");
    let installed = temp.path().join("bin/shoal");
    let invoked = Invoked::at(installed.clone());

    let replacement = Replacement::of(Some(&releases), executable.clone(), &invoked).unwrap();

    assert_eq!(replacement.program, executable);
    assert_eq!(
        variable(&replacement, env::RELEASE_INVOKED),
        Some(&installed.into_os_string())
    );
}

#[test]
fn a_bare_program_runs_as_it_is_without_a_copy() {
    let temp = tempfile::tempdir().unwrap();
    let releases = Releases::new(temp.path().join("releases"));
    let executable = temp.path().join("shoal");
    fs::write(&executable, "build").unwrap();
    let invoked = Invoked::at(executable.clone());

    let replacement = Replacement::of(Some(&releases), executable.clone(), &invoked).unwrap();

    assert_eq!(replacement.program, executable);
    assert_eq!(
        variable(&replacement, env::RELEASE_LOCKS),
        Some(&OsString::new())
    );
}
