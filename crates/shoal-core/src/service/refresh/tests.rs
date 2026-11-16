use super::*;
use std::path::Path;

/// The unit this daemon would install, at a path in `root`.
fn current(paths: &Paths, root: &Path) -> Definition {
    let rendered = definition(paths, Path::new("/usr/local/bin/shoal"), Platform::Linux).unwrap();
    Definition {
        path: root.join("shoal.service"),
        ..rendered
    }
}

/// `unit` with the line systemd's OOM policy takes left out, as Shoal
/// installed it before it set one.
fn without_oom_policy(unit: &str) -> String {
    unit.lines()
        .filter(|line| !line.starts_with("OOMPolicy="))
        .map(|line| format!("{line}\n"))
        .collect()
}

#[test]
fn an_outdated_unit_of_this_state_directory_is_rewritten() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let current = current(&paths, root.path());
    fs::write(&current.path, without_oom_policy(&current.content)).unwrap();
    assert!(outdated(&paths, &current, Platform::Linux).unwrap());
}

#[test]
fn a_current_unit_is_left_alone() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let current = current(&paths, root.path());
    fs::write(&current.path, &current.content).unwrap();
    assert!(!outdated(&paths, &current, Platform::Linux).unwrap());
}

#[test]
fn a_unit_of_another_state_directory_is_left_alone() {
    let root = tempfile::tempdir().unwrap();
    let other = Paths::for_test(root.path().join("other"));
    let installed = current(&other, root.path());
    fs::write(&installed.path, without_oom_policy(&installed.content)).unwrap();
    let paths = Paths::for_test(root.path());
    assert!(!outdated(&paths, &current(&paths, root.path()), Platform::Linux).unwrap());
}

#[test]
fn no_unit_is_installed_by_a_refresh() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let current = current(&paths, root.path());
    assert!(!rewrite(&paths, &current, Platform::Linux).unwrap());
    assert!(!current.path.exists());
}

#[test]
fn only_a_build_that_changes_user_files_rewrites_the_unit() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let current = current(&paths, root.path());
    let outdated = without_oom_policy(&current.content);
    fs::write(&current.path, &outdated).unwrap();
    let rewritten = rewrite(&paths, &current, Platform::Linux).unwrap();
    assert_eq!(rewritten, crate::build::changes_user_files());
    let expected = if rewritten {
        &current.content
    } else {
        &outdated
    };
    assert_eq!(&fs::read_to_string(&current.path).unwrap(), expected);
}
