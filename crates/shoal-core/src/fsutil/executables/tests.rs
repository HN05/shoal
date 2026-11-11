use super::*;
use std::os::unix::fs::symlink;

#[test]
fn invoked_path_keeps_an_installation_symlink_to_the_running_binary() {
    let root = tempfile::tempdir().unwrap();
    let keg = root.path().join("keg");
    let bin = root.path().join("bin");
    fs::create_dir_all(&keg).unwrap();
    fs::create_dir_all(&bin).unwrap();
    let binary = keg.join("shoal");
    let other = keg.join("other");
    for file in [&binary, &other] {
        fs::write(file, "").unwrap();
        fs::set_permissions(file, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let link = bin.join("shoal");
    std::os::unix::fs::symlink(&binary, &link).unwrap();
    let resolve = |arg: &str| {
        invoked_path(
            Some(arg.into()),
            bin.as_os_str(),
            root.path(),
            binary.clone(),
        )
    };
    assert_eq!(resolve(link.to_str().unwrap()), link);
    assert_eq!(resolve("shoal"), link, "found on PATH");
    assert_eq!(
        resolve("bin/shoal"),
        link,
        "relative to the current directory"
    );
    assert_eq!(
        invoked_path(
            Some("shoal".into()),
            "bin".as_ref(),
            root.path(),
            binary.clone()
        ),
        link,
        "found through a relative PATH entry"
    );
    assert_eq!(resolve(other.to_str().unwrap()), binary);
    assert_eq!(resolve("missing"), binary);
    assert_eq!(
        invoked_path(None, bin.as_os_str(), root.path(), binary.clone()),
        binary
    );
}

#[test]
fn executable_search_skips_invalid_candidates_and_preserves_symlinks() {
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("first");
    let second = root.path().join("second");
    fs::create_dir(&first).unwrap();
    fs::create_dir(&second).unwrap();
    let program = second.join("tool");
    fs::write(&program, "executable").unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o100)).unwrap();
    let search = std::env::join_paths([&first, &second]).unwrap();
    let candidate = first.join("tool");
    assert!(is_executable(&candidate).is_err());
    for directory in [false, true] {
        if directory {
            fs::remove_file(&candidate).unwrap();
            fs::create_dir(&candidate).unwrap();
        } else {
            fs::write(&candidate, "not executable").unwrap();
            fs::set_permissions(&candidate, fs::Permissions::from_mode(0o600)).unwrap();
        }
        assert!(!is_executable(&candidate).unwrap());
        assert_eq!(
            find_executable(OsStr::new("tool"), &search),
            Some(program.clone())
        );
    }
    fs::remove_dir(&candidate).unwrap();
    symlink(&program, &candidate).unwrap();
    assert_eq!(
        find_executable(OsStr::new("tool"), &search),
        Some(candidate)
    );
    assert_eq!(find_executable(OsStr::new("missing"), &search), None);
}
