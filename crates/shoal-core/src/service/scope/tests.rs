use super::*;

#[test]
fn the_launcher_starts_a_scope_in_its_slice() {
    let launcher = scope_command(vec!["systemd-run".into()], Some("workloads.slice"));
    assert!(launcher.contains(&"--scope".into()), "{launcher:?}");
    assert!(
        launcher.contains(&"--slice=workloads.slice".into()),
        "{launcher:?}"
    );
    assert!(
        launcher.contains(&"--property=OOMPolicy=continue".into()),
        "{launcher:?}"
    );
    assert_eq!(launcher.last(), Some(&"--".into()));
}

#[test]
fn a_launcher_without_a_slice_names_none() {
    let launcher = scope_command(vec!["systemd-run".into()], None);
    assert!(
        !(launcher.iter()).any(|arg| arg.to_string_lossy().starts_with("--slice")),
        "{launcher:?}"
    );
}

#[test]
fn only_a_systemd_that_can_leave_arguments_unexpanded_starts_scopes() {
    assert!(keeps_arguments("systemd 262 (262.1-1.fc45)\n+PAM +AUDIT"));
    assert!(keeps_arguments("systemd 254 (254.5-2)"));
    assert!(!keeps_arguments("systemd 252 (252.30-1~deb12u2)"));
    assert!(!keeps_arguments("unexpected"));
}

#[test]
fn systemd_run_keeps_this_processs_user_bus_in_any_environment() {
    use std::os::unix::fs::PermissionsExt as _;
    let bin = tempfile::tempdir().unwrap();
    for program in ["env", "systemd-run"] {
        let file = bin.path().join(program);
        std::fs::write(&file, "").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let variable = |name: &str| (name == "XDG_RUNTIME_DIR").then(|| "/run/user/1000".into());
    let command = systemd_run_with_bus(bin.path().as_os_str(), variable).unwrap();
    assert_eq!(
        command,
        [
            bin.path().join("env").into_os_string(),
            "XDG_RUNTIME_DIR=/run/user/1000".into(),
            bin.path().join("systemd-run").into_os_string(),
        ]
    );
}
