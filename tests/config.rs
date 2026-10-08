mod support;

use std::{fs, path::Path, process::Command};

fn command(home: &Path) -> Command {
    let mut command = support::cli(home);
    command.env("XDG_CONFIG_HOME", home.join("xdg"));
    command
}

#[test]
fn inline_edits_validate_preserve_comments_and_work_without_a_daemon() {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("xdg/shoal/config.toml");
    let run = |args: &[&str]| command(home.path()).args(args).output().unwrap();
    let output = run(&["--json", "config", "set", "default_agent", "codex"]);
    assert!(output.status.success(), "{:?}", output);
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["config"], config.to_str().unwrap());
    assert!(result["backup"].is_null());
    assert_eq!(result["daemon_reloaded"], false);
    let original =
        "# policy\ndefault_agent = 'codex' # chosen\n[ports]\nstart = 3000\nend = 4000\n";
    fs::write(&config, original).unwrap();
    let output = run(&["config", "set", "default_agent", "claude"]);
    assert!(output.status.success(), "{:?}", output);
    assert_eq!(
        fs::read_to_string(config.with_extension("toml.backup")).unwrap(),
        original
    );
    let saved = fs::read_to_string(&config).unwrap();
    assert!(saved.contains("# chosen"));
    assert!(saved.contains("# policy"));
    for args in [
        vec!["config", "set", "ports.start", "5000"],
        vec!["config", "set", "auto_cleanup.enabled", "maybe"],
        vec!["config", "set", "default_agnet", "codex"],
        vec!["config", "unset", "missing"],
        vec!["config", "set", "default_agent", "claude", "ports.start"],
        vec!["config", "unset", "default_agent", "missing"],
    ] {
        assert!(!run(&args).status.success(), "{args:?}");
        assert_eq!(fs::read_to_string(&config).unwrap(), saved);
        assert_eq!(
            fs::read_to_string(config.with_extension("toml.backup")).unwrap(),
            original
        );
    }
    assert!(run(&["config", "unset", "default_agent"]).status.success());
    assert!(
        !fs::read_to_string(&config)
            .unwrap()
            .contains("default_agent")
    );
    assert!(!home.path().join("state").exists());
}

#[test]
fn scoped_inline_edits_leave_config_untouched() {
    let home = tempfile::tempdir().unwrap();
    for args in [
        vec!["config", "set", "default_agent", "codex"],
        vec!["config", "unset", "default_agent"],
    ] {
        let output = command(home.path())
            .args(args)
            .env("SHOAL_SCOPE_TOKEN", "test-scope")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("cannot administer"));
    }
    assert!(!home.path().join("xdg").exists());
}

#[test]
fn clearing_scope_in_a_child_keeps_the_ancestor_scope_for_its_state() {
    let home = tempfile::tempdir().unwrap();
    // This test binary is the scoped parent: macOS hides the environment of
    // platform binaries such as /bin/sh, as it would an agent's shell.
    let run = |args: &str, state: Option<&Path>| {
        let mut parent = support::isolated(home.path(), std::env::current_exe().unwrap());
        parent
            .args(["--exact", "unscoped_child", "--ignored", "--nocapture"])
            .env("XDG_CONFIG_HOME", home.path().join("xdg"))
            .env("SHOAL_SCOPE_TOKEN", "test-scope")
            .env("CHILD_ARGS", args);
        if let Some(state) = state {
            parent.env("CHILD_STATE_DIR", state);
        }
        parent.output().unwrap()
    };
    let config = "config set default_agent codex";
    for (args, refusal) in [
        (config, "cannot administer"),
        ("skill install", "cannot install user-level skills"),
        ("shell recover /", "cannot navigate outside their worktree"),
    ] {
        let output = run(args, None);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains(refusal));
    }
    assert!(!home.path().join("xdg").exists());
    let output = run(config, Some(&home.path().join("other-state")));
    assert!(output.status.success(), "{output:?}");
}

/// Runs `CHILD_ARGS` with the scope token cleared, as an agent's child could,
/// and exits with its status.
#[test]
#[ignore = "helper launched by clearing_scope_in_a_child_keeps_the_ancestor_scope_for_its_state"]
fn unscoped_child() {
    let mut command = Command::new(env!("CARGO_BIN_EXE_shoal"));
    command
        .args(std::env::var("CHILD_ARGS").unwrap().split_whitespace())
        .env_remove("SHOAL_SCOPE_TOKEN");
    if let Some(state) = std::env::var_os("CHILD_STATE_DIR") {
        command.env("SHOAL_STATE_DIR", state);
    }
    std::process::exit(command.status().unwrap().code().unwrap_or(1));
}

#[test]
fn install_works_offline_and_backs_up_the_existing_config() {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("xdg/shoal/config.toml");
    let backup = config.with_extension("toml.backup");
    let template = include_str!("../configs/default.toml");
    for previous in [None, Some("invalid TOML ["), Some("# another edit\n")] {
        if let Some(previous) = previous {
            fs::write(&config, previous).unwrap();
        }
        let output = command(home.path())
            .args(["--json", "config", "install", "default"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["name"], "default");
        assert_eq!(result["config"], config.to_str().unwrap());
        if let Some(previous) = previous {
            assert_eq!(result["backup"], backup.to_str().unwrap());
            assert_eq!(fs::read_to_string(&backup).unwrap(), previous);
        } else {
            assert!(result["backup"].is_null());
            assert!(!backup.exists());
        }
        assert_eq!(fs::read_to_string(&config).unwrap(), template);
    }
    assert!(!home.path().join("state").exists());
}

#[test]
fn unknown_names_and_scoped_calls_leave_config_and_backup_untouched() {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("xdg/shoal/config.toml");
    let backup = config.with_extension("toml.backup");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(&config, "keep config").unwrap();
    fs::write(&backup, "keep backup").unwrap();
    for name in ["missing", "../default", "/tmp/default", "default.toml"] {
        let output = command(home.path())
            .args(["config", "install", name])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("default"));
    }
    let output = command(home.path())
        .args(["config", "install", "default"])
        .env("SHOAL_SCOPE_TOKEN", "test-scope")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot administer"));
    assert_eq!(fs::read_to_string(config).unwrap(), "keep config");
    assert_eq!(fs::read_to_string(backup).unwrap(), "keep backup");
    assert!(!home.path().join("state").exists());
}

#[test]
fn install_preserves_symlink_targets_and_refuses_directories() {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("xdg/shoal/config.toml");
    let original = home.path().join("personal.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    fs::write(&original, "personal config").unwrap();
    std::os::unix::fs::symlink(&original, &config).unwrap();
    let run = || {
        command(home.path())
            .args(["config", "install", "default"])
            .output()
            .unwrap()
    };
    assert!(run().status.success());
    assert_eq!(fs::read_to_string(&original).unwrap(), "personal config");
    assert_eq!(
        fs::read_link(config.with_extension("toml.backup")).unwrap(),
        original
    );
    assert!(!config.is_symlink());
    fs::remove_file(&config).unwrap();
    fs::create_dir(&config).unwrap();
    fs::write(config.join("keep"), "keep").unwrap();
    assert!(!run().status.success());
    assert_eq!(fs::read_to_string(config.join("keep")).unwrap(), "keep");
    assert_eq!(
        fs::read_to_string(config.with_extension("toml.backup")).unwrap(),
        "personal config"
    );
}
