use super::*;

#[test]
fn a_wrapper_continues_only_from_the_binary_the_daemon_runs() {
    let root = tempfile::tempdir().unwrap();
    let release = root.path().join("libexec/shoal");
    std::fs::create_dir_all(release.parent().unwrap()).unwrap();
    std::fs::write(&release, "release").unwrap();
    let daemon = FileIdentity::of(&release).ok();
    // The daemon started from its versioned path; the wrapper's candidate is
    // the link to it.
    let link = root.path().join("bin/shoal");
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&release, &link).unwrap();
    assert!(runs_daemon_binary(&link, daemon));
    // The link now names a newer release than the one the daemon runs.
    let next = root.path().join("next/shoal");
    std::fs::create_dir_all(next.parent().unwrap()).unwrap();
    std::fs::write(&next, "next release").unwrap();
    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(&next, &link).unwrap();
    assert!(!runs_daemon_binary(&link, daemon));
    // A daemon that does not say which binary it runs allows none.
    assert!(!runs_daemon_binary(&release, None));
}
