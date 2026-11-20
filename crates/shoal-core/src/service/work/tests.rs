use super::*;

#[test]
fn a_cap_throttles_at_the_limit_and_kills_a_twentieth_above_it() {
    let cap = Cap::of(20 << 30);
    assert_eq!(
        properties(Some(cap)),
        ["MemoryHigh=21474836480", "MemoryMax=22548578304"]
    );
    assert_eq!(
        properties(None),
        ["MemoryHigh=infinity", "MemoryMax=infinity"]
    );
}

#[test]
fn the_slice_lies_below_the_users_service_manager() {
    let terminal = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/rex.service/t-1\n";
    assert_eq!(
        manager_group(Some(terminal), 1000),
        "user.slice/user-1000.slice/user@1000.service"
    );
    // A login session's processes lie outside the manager's group.
    let login = "0::/user.slice/user-1000.slice/session-3.scope\n";
    for own in [Some(login), None] {
        assert_eq!(
            manager_group(own, 1000),
            "user.slice/user-1000.slice/user@1000.service"
        );
    }
}

#[test]
fn a_process_in_a_scope_of_the_slice_is_in_it() {
    let scoped = "0::/user.slice/user-1000.slice/user@1000.service/shoal.slice/shoal-work.slice/run-p1-i2.scope\n";
    let terminal = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/rex.service/t-1\n";
    assert!(in_slice(scoped));
    assert!(!in_slice(terminal));
}

#[test]
fn a_launched_command_gets_back_the_bus_its_environment_sets() {
    let launcher: Vec<std::ffi::OsString> = [
        "/usr/bin/env",
        "XDG_RUNTIME_DIR=/run/user/1000",
        "DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus",
        "/usr/bin/systemd-run",
        "--scope",
        "--",
    ]
    .map(Into::into)
    .to_vec();
    let mut command = tokio::process::Command::new("agent");
    command
        .arg("--flag")
        .env("XDG_RUNTIME_DIR", "/private")
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
        .env("OTHER", "kept")
        .current_dir("/work");
    let launched = launched(command, &launcher);
    let launched = launched.as_std();
    let argv: Vec<_> = std::iter::once(launched.get_program())
        .chain(launched.get_args())
        .collect();
    let expected: Vec<&std::ffi::OsStr> = launcher
        .iter()
        .map(|arg| arg.as_os_str())
        .chain(
            [
                "/usr/bin/env",
                "-u",
                "DBUS_SESSION_BUS_ADDRESS",
                "XDG_RUNTIME_DIR=/private",
                "agent",
                "--flag",
            ]
            .map(std::ffi::OsStr::new),
        )
        .collect();
    assert_eq!(argv, expected);
    assert!(
        (launched.get_envs())
            .any(|(name, value)| name == "OTHER" && value == Some("kept".as_ref()))
    );
    assert_eq!(
        launched.get_current_dir(),
        Some(std::path::Path::new("/work"))
    );
}

#[test]
fn a_command_that_keeps_this_processs_bus_starts_right_after_the_launcher() {
    let launcher: Vec<std::ffi::OsString> = ["/usr/bin/env", "/usr/bin/systemd-run", "--"]
        .map(Into::into)
        .to_vec();
    let through = launched(tokio::process::Command::new("agent"), &launcher);
    let args: Vec<_> = through.as_std().get_args().collect();
    assert_eq!(args, ["/usr/bin/systemd-run", "--", "agent"]);
    // Without a launcher it starts directly.
    let direct = launched(tokio::process::Command::new("agent"), &[]);
    assert_eq!(direct.as_std().get_program(), "agent");
}
