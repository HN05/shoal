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

#[tokio::test(start_paused = true)]
async fn a_preparation_that_hangs_gives_up_at_its_limit() {
    let started = tokio::time::Instant::now();
    let error = within_preparation_limit(std::future::pending::<Result<()>>())
        .await
        .unwrap_err();
    assert_eq!(started.elapsed(), PREPARATION_LIMIT);
    assert!(format!("{error}").contains("shoal resume"), "{error}");
}

#[test]
fn an_agent_without_a_resume_command_is_not_taken_over() {
    let recovery = |command: Vec<std::ffi::OsString>, automatic| super::super::recovery::Recovery {
        model: Default::default(),
        agent: "custom".into(),
        command,
        automatic,
        handoff_delivered: false,
        proxy: None,
    };
    let error = ensure_restorable(&recovery(vec![], false)).unwrap_err();
    assert!(format!("{error}").contains("shoal resume"), "{error}");
    assert!(ensure_restorable(&recovery(vec!["./resume.sh".into()], true)).is_ok());
}
