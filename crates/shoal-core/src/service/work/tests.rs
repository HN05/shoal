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
    let launched = launched(command, &launcher, None);
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
    let through = launched(tokio::process::Command::new("agent"), &launcher, None);
    let args: Vec<_> = through.as_std().get_args().collect();
    assert_eq!(args, ["/usr/bin/systemd-run", "--", "agent"]);
    // Without a launcher it starts directly.
    let direct = launched(tokio::process::Command::new("agent"), &[], None);
    assert_eq!(direct.as_std().get_program(), "agent");
}

#[test]
fn a_spaces_work_runs_in_a_scope_named_for_it() {
    let launcher: Vec<std::ffi::OsString> = ["/usr/bin/systemd-run", "--scope", "--"]
        .map(Into::into)
        .to_vec();
    let space = SpaceId::from("3dd4fd2c-1a61-4797-a359-6f1ec40e8d71");
    let unit = |space: &SpaceId| {
        let through = launched(
            tokio::process::Command::new("agent"),
            &launcher,
            Some(space),
        );
        let args: Vec<_> = (through.as_std().get_args())
            .map(|arg| arg.to_str().unwrap().to_owned())
            .collect();
        assert_eq!(args[0], "--scope");
        assert_eq!(args[args.len() - 2..], ["--", "agent"]);
        (args.len() == 4).then(|| args[1].strip_prefix("--unit=").unwrap().to_owned())
    };
    let first = unit(&space).unwrap();
    let groups = |unit: &str| {
        format!(
            "0::/user.slice/user-1000.slice/user@1000.service/shoal.slice/shoal-work.slice/{unit}\n"
        )
    };
    assert_eq!(scope_space(&groups(&first)), Some(space.as_str()));
    // Each command of the space gets a scope of its own.
    assert_ne!(unit(&space).unwrap(), first);
    // A launcher whose options end with no `--` takes no name.
    let plain: Vec<std::ffi::OsString> = ["env", "A=1"].map(Into::into).to_vec();
    let through = launched(tokio::process::Command::new("agent"), &plain, Some(&space));
    let args: Vec<_> = through.as_std().get_args().collect();
    assert_eq!(args, ["A=1", "agent"]);
    // An ID a unit's name cannot hold leaves the name to systemd.
    assert_eq!(unit(&SpaceId::from("a b")), None);
    assert_eq!(scope_space(&groups("run-p1-i2.scope")), None);
}
