mod support;

use std::{fs, io::Read, os::unix::fs::PermissionsExt, process::Command};

#[test]
fn installed_scripts_preserve_literal_paths_contents_and_modes() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let script = root.path().join("nested ' $literal/bin/test script");
    let contents = "#!/bin/sh\nprintf '%s' \"$1\"\n";
    support::install_test_script(&script, contents);
    assert_eq!(fs::read_to_string(&script).unwrap(), contents);
    assert_eq!(
        fs::metadata(&script).unwrap().permissions().mode() & 0o777,
        0o755
    );
    let output = support::isolated(root.path(), &script)
        .arg("quote ' ; $(touch injected)")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"quote ' ; $(touch injected)");
    assert!(!root.path().join("injected").exists());

    // An executing fixture keeps the previous file even after replacement.
    let mut previous = fs::File::open(&script).unwrap();
    support::install_test_script_with_mode(&script, "#!/bin/sh\nexit 7\n", 0o700);
    let mut previous_contents = String::new();
    previous.read_to_string(&mut previous_contents).unwrap();
    assert_eq!(previous_contents, contents);
    assert_eq!(
        previous.metadata().unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert_eq!(
        fs::metadata(&script).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        support::isolated(root.path(), &script)
            .status()
            .unwrap()
            .code(),
        Some(7)
    );
}

#[test]
fn processes_ignore_launching_environment_and_allow_explicit_overrides() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let outside = root.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("config.toml"), "invalid config").unwrap();
    // Pollute a subprocess running the probe, never the parallel test runner.
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "isolation_probe", "--nocapture"])
        .env("SHOAL_FIXTURE_PROBE", root.path())
        .env("HOME", &outside)
        .env("SHOAL_STATE_DIR", &outside)
        .env("SHOAL_SCOPE_TOKEN", "inherited-scope")
        .env("SHOAL_EXECUTION_ID", "inherited-execution")
        .env("SHOAL_COMPLETE", "bash")
        .env("SHOAL_FUTURE_SETTING", "inherited")
        .env("XDG_CONFIG_HOME", &outside)
        .env("XDG_STATE_HOME", &outside)
        .env("CLAUDE_CONFIG_DIR", &outside)
        .env("CODEX_HOME", &outside)
        .env("HAPPY_HOME_DIR", &outside)
        .env("HAPPY_SERVER_URL", "http://invalid")
        .env("GIT_DIR", &outside)
        .env("GIT_CONFIG_GLOBAL", outside.join("config.toml"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(fs::read_dir(outside).unwrap().count(), 1);
}

#[test]
fn isolation_probe() {
    let Some(root) = std::env::var_os("SHOAL_FIXTURE_PROBE") else {
        return;
    };
    let root = std::path::Path::new(&root);
    let output = support::isolated(root, "sh")
        .args(["-c", "env"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let environment = String::from_utf8(output.stdout).unwrap();
    for name in [
        "SHOAL_SCOPE_TOKEN",
        "SHOAL_EXECUTION_ID",
        "SHOAL_COMPLETE",
        "SHOAL_FUTURE_SETTING",
        "XDG_CONFIG_HOME",
        "XDG_STATE_HOME",
        "CLAUDE_CONFIG_DIR",
        "CODEX_HOME",
        "HAPPY_HOME_DIR",
        "HAPPY_SERVER_URL",
        "GIT_DIR",
    ] {
        assert!(
            !environment
                .lines()
                .any(|line| line.starts_with(&format!("{name}="))),
            "{name}"
        );
    }
    assert!(
        environment
            .lines()
            .any(|line| line == format!("HOME={}", root.display()))
    );
    assert!(
        environment
            .lines()
            .any(|line| line == format!("SHOAL_STATE_DIR={}/state", root.display()))
    );
    assert_eq!(support::cli(root).get_current_dir(), Some(root));
    let output = support::cli(root)
        .args(["config", "install", "default"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let output = support::cli(root)
        .env("SHOAL_SCOPE_TOKEN", "explicit-scope")
        .args(["config", "install", "default"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot administer"));
    assert!(!root.join("state").exists());
}
