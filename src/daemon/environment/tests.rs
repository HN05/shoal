use super::*;
use crate::test_support::install_test_script;
use std::fs;

#[tokio::test]
async fn root_dir_reports_whether_the_daemon_can_write_there() {
    let root = tempfile::tempdir().unwrap();
    let check = root_dir(Ok(root.path().join("shoal"))).await;
    assert_eq!(check.status, CheckStatus::Ok);
    let file = root.path().join("file");
    fs::write(&file, "").unwrap();
    let check = root_dir(Ok(root.path().join("missing/../file"))).await;
    assert_eq!(check.status, CheckStatus::Error);
    let check = root_dir(Ok(file.clone())).await;
    assert_eq!(check.status, CheckStatus::Error);
    assert!(
        check.message.starts_with(&format!(
            "Cannot use root_dir: the Shoal daemon cannot write to {}: ",
            fs::canonicalize(&file).unwrap().display()
        )),
        "{}",
        check.message
    );
}

#[test]
fn dependencies_require_executable_files_on_the_supplied_path() {
    let root = tempfile::tempdir().unwrap();
    install_test_script(&root.path().join("git"), "#!/bin/sh\n");
    fs::write(root.path().join("wt"), "not executable").unwrap();
    fs::create_dir(root.path().join("lsof")).unwrap();
    let checks = dependencies(Some(root.path().as_os_str()), HostName::Zmx);
    assert_eq!(checks[0].status, CheckStatus::Ok);
    assert_eq!(checks[1].status, CheckStatus::Error);
    assert_eq!(checks[2].status, CheckStatus::Error);
    assert_eq!(checks[3].status, CheckStatus::Error);
    assert!(
        checks[3].message.contains("terminal.default_host"),
        "{}",
        checks[3].message
    );
    assert_eq!(checks[4].status, CheckStatus::Warning);
    assert!(
        dependencies(None, HostName::Zmx)
            .iter()
            .all(|c| c.status != CheckStatus::Ok)
    );
}

#[test]
fn only_the_default_terminal_host_is_required() {
    let checks = dependencies(None, HostName::Herdr);
    assert!(
        checks
            .iter()
            .all(|check| !check.message.starts_with("zmx ")),
        "zmx is checked although Herdr is the default host"
    );
    assert_eq!(checks.len(), dependencies(None, HostName::Zmx).len() - 1);
}
