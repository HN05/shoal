use super::*;
use crate::service::definition::Launch;

/// What `shoal daemon install` chose for the unit the tests install, other
/// than what the daemon running the tests has.
fn installed_launch() -> Launch {
    Launch {
        executable: "/opt/a b/%n/$shoal".into(),
        search_path: "/opt/\"bin\":/usr/bin".into(),
    }
}

/// The unit this release renders for `launch`, at `path`.
fn unit(paths: &Paths, launch: &Launch, path: &Path) -> Definition {
    Definition {
        path: path.to_owned(),
        ..render(paths, launch, Platform::Linux).unwrap()
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
fn an_outdated_unit_is_rendered_again_for_what_it_starts() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let current = unit(
        &paths,
        &installed_launch(),
        &root.path().join("shoal.service"),
    );
    fs::write(&current.path, without_oom_policy(&current.content)).unwrap();
    let replacement = replacement(&paths, &current.path, Platform::Linux).unwrap();
    assert_eq!(replacement.map(|d| d.content), Some(current.content));
}

#[test]
fn a_current_unit_is_left_alone() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let current = unit(
        &paths,
        &installed_launch(),
        &root.path().join("shoal.service"),
    );
    fs::write(&current.path, &current.content).unwrap();
    let replacement = replacement(&paths, &current.path, Platform::Linux).unwrap();
    assert!(replacement.is_none());
}

#[test]
fn a_unit_of_another_state_directory_is_left_alone() {
    let root = tempfile::tempdir().unwrap();
    let other = Paths::for_test(root.path().join("other"));
    let path = root.path().join("shoal.service");
    let installed = unit(&other, &installed_launch(), &path);
    fs::write(&path, without_oom_policy(&installed.content)).unwrap();
    let paths = Paths::for_test(root.path());
    assert!(
        replacement(&paths, &path, Platform::Linux)
            .unwrap()
            .is_none()
    );
}

#[test]
fn no_unit_is_installed_by_a_refresh() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let path = root.path().join("shoal.service");
    assert!(
        replacement(&paths, &path, Platform::Linux)
            .unwrap()
            .is_none()
    );
}

#[test]
fn only_a_build_that_changes_user_files_rewrites_the_unit() {
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_test(root.path());
    let current = unit(
        &paths,
        &installed_launch(),
        &root.path().join("shoal.service"),
    );
    let outdated = without_oom_policy(&current.content);
    fs::write(&current.path, &outdated).unwrap();
    let rewritten = rewrite(&current).unwrap();
    assert_eq!(rewritten, crate::build::changes_user_files());
    let expected = if rewritten {
        &current.content
    } else {
        &outdated
    };
    assert_eq!(&fs::read_to_string(&current.path).unwrap(), expected);
}
