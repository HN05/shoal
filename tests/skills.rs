mod support;

use std::{
    fs,
    os::unix::fs::symlink,
    path::Path,
    process::{Command, Output},
};

use serde_json::json;

fn cli(home: &Path) -> Command {
    let mut command = support::cli(home);
    command
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("SHOAL_DEV_USER_FILES", "1")
        // Skill delivery must not depend on socket path length or daemon setup.
        .env("SHOAL_STATE_DIR", home.join("x".repeat(150)));
    command
}

const WORKER: &[u8] = include_bytes!("../skills/shoal-worker/SKILL.md");
const SKILLS: [(&str, &[u8]); 2] = [
    ("shoal-worker", WORKER),
    (
        "shoal-orchestrator",
        include_bytes!("../skills/shoal-orchestrator/SKILL.md"),
    ),
];

fn write_skills(directory: &Path, contents: &str) {
    for (name, _) in SKILLS {
        fs::create_dir_all(directory.join(name)).unwrap();
        fs::write(directory.join(name).join("SKILL.md"), contents).unwrap();
    }
}

fn assert_installed(directory: &Path) {
    for (name, contents) in SKILLS {
        assert_eq!(
            fs::read(directory.join(name).join("SKILL.md")).unwrap(),
            contents
        );
    }
}

/// Codex skips a linked `SKILL.md`, so installed skills are regular files.
fn assert_copies(directory: &Path, contents: &str) {
    for (name, _) in SKILLS {
        let path = directory.join(name).join("SKILL.md");
        assert!(fs::symlink_metadata(&path).unwrap().is_file(), "{path:?}");
        assert_eq!(fs::read_to_string(path).unwrap(), contents);
    }
}

fn success(output: Output) -> Vec<u8> {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn copy_executable(home: &Path, destination: &Path) {
    // Copy in a child so parallel command spawns cannot inherit the writable
    // file descriptor and cause ETXTBSY when this test executes the fixture.
    success(
        support::isolated(home, "cp")
            .arg(env!("CARGO_BIN_EXE_shoal"))
            .arg(destination)
            .output()
            .unwrap(),
    );
}

#[test]
fn packaged_binary_runs_from_deleted_cwd_and_installs_skill_copies() {
    let home = tempfile::tempdir_in("/tmp").unwrap();
    let package = home.path().join("version one");
    let stable = home.path().join("opt");
    let bin = home.path().join("bin");
    fs::create_dir_all(package.join("libexec")).unwrap();
    fs::create_dir(&bin).unwrap();
    copy_executable(home.path(), &package.join("libexec/shoal"));
    write_skills(&package.join("skills"), "version one");
    symlink(&package, &stable).unwrap();
    symlink(stable.join("skills"), package.join("libexec/shoal-skills")).unwrap();
    symlink(package.join("libexec/shoal"), bin.join("shoal")).unwrap();

    for args in [
        vec!["--json", "daemon", "status"],
        vec!["skill", "install", "codex"],
    ] {
        let deleted = home.path().join("deleted");
        fs::create_dir(&deleted).unwrap();
        let output = support::isolated(home.path(), "bash")
            .env("SHOAL_DEV_USER_FILES", "1")
            .args([
                "--noprofile",
                "--norc",
                "-c",
                "cd -- \"$1\" && rmdir -- \"$1\" || exit; shift; exec \"$@\"",
                "deleted-cwd-test",
            ])
            .arg(&deleted)
            .arg(bin.join("shoal"))
            .args(&args)
            .output()
            .unwrap();
        assert!(output.stderr.is_empty(), "{output:?}");
        if args.contains(&"daemon") {
            // No daemon runs in this home, which status reports without failing to start.
            assert_eq!(output.status.code(), Some(1));
            let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(status["running"], false);
        } else {
            success(output);
        }
    }
    let installed = home.path().join(".agents/skills");
    assert_copies(&installed, "version one");
    let upgraded = home.path().join("version two");
    write_skills(&upgraded.join("skills"), "version two");
    fs::remove_file(&stable).unwrap();
    symlink(&upgraded, &stable).unwrap();
    // The next command copies the upgraded package's skills.
    success(
        support::isolated(home.path(), bin.join("shoal"))
            .env("SHOAL_DEV_USER_FILES", "1")
            .arg("skill")
            .output()
            .unwrap(),
    );
    assert_copies(&installed, "version two");

    // The explicit runtime source still overrides the adjacent package link.
    success(
        support::isolated(home.path(), bin.join("shoal"))
            .env("SHOAL_DEV_USER_FILES", "1")
            .env("SHOAL_SKILLS_DIR", package.join("skills"))
            .args(["skill", "install", "codex"])
            .output()
            .unwrap(),
    );
    assert_copies(&installed, "version one");
    assert!(!home.path().join("state").exists());
}

#[test]
fn relative_packaged_skill_link_installs_copies_that_follow_homebrew_upgrades() {
    let home = tempfile::tempdir().unwrap();
    let prefix = fs::canonicalize(home.path()).unwrap();
    let cellar = prefix.join("Cellar/shoal");
    let skills = Path::new("share/shoal/skills");
    fs::create_dir_all(prefix.join("opt")).unwrap();
    fs::create_dir(prefix.join("bin")).unwrap();
    let stable = prefix.join("opt/shoal");
    let launcher = prefix.join("bin/shoal");
    let installed = home.path().join(".agents/skills");
    // Homebrew links the launcher into the versioned keg and the stable prefix to it.
    for (version, args) in [
        ("0.5.0", ["skill", "install", "codex"]),
        ("0.5.1", ["skill", "orchestrator", "--json"]),
    ] {
        let package = cellar.join(version);
        write_skills(&package.join(skills), version);
        fs::create_dir_all(package.join("libexec")).unwrap();
        copy_executable(home.path(), &package.join("libexec/shoal"));
        symlink(
            "../../../../opt/shoal/share/shoal/skills",
            package.join("libexec/shoal-skills"),
        )
        .unwrap();
        for link in [&stable, &launcher] {
            let _ = fs::remove_file(link);
        }
        symlink(format!("../Cellar/shoal/{version}"), &stable).unwrap();
        symlink(
            format!("../Cellar/shoal/{version}/libexec/shoal"),
            &launcher,
        )
        .unwrap();
        success(
            support::isolated(home.path(), &launcher)
                .env("SHOAL_DEV_USER_FILES", "1")
                .args(args)
                .output()
                .unwrap(),
        );
        assert_copies(&installed, version);
    }
}

#[test]
fn skill_export_and_default_install_work_without_daemon_or_repository() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(
        success(cli(home.path()).arg("skill").output().unwrap()),
        WORKER
    );
    for (argument, (name, contents)) in ["worker", "orchestrator"].into_iter().zip(SKILLS) {
        let exported: serde_json::Value = serde_json::from_slice(&success(
            cli(home.path())
                .args(["--json", "skill", argument])
                .output()
                .unwrap(),
        ))
        .unwrap();
        assert_eq!(exported["name"], name);
        assert_eq!(exported["skill"].as_str().unwrap().as_bytes(), contents);
    }
    assert_eq!(fs::read_dir(home.path()).unwrap().count(), 0);
    // Installing for every tool writes nothing until a skill directory exists.
    let output = success(
        cli(home.path())
            .args(["skill", "install"])
            .output()
            .unwrap(),
    );
    assert!(String::from_utf8_lossy(&output).contains("No skill directories found"));
    assert_eq!(fs::read_dir(home.path()).unwrap().count(), 0);
    // The single skill earlier versions installed is replaced.
    let retired = home.path().join(".agents/skills/shoal");
    fs::create_dir_all(&retired).unwrap();
    fs::write(retired.join("SKILL.md"), "retired").unwrap();
    fs::create_dir_all(home.path().join(".claude/skills")).unwrap();
    let installed: serde_json::Value = serde_json::from_slice(&success(
        cli(home.path())
            .args(["--json", "skill", "install"])
            .output()
            .unwrap(),
    ))
    .unwrap();
    let entries = installed["installed"].as_array().unwrap();
    // opencode and pi share Codex's directory, reported once; Grok's does not
    // exist.
    assert_eq!(entries.len(), 4);
    for (agents, relative) in [
        (&["codex", "opencode", "pi"][..], ".agents/skills"),
        (&["claude"], ".claude/skills"),
    ] {
        let directory = home.path().join(relative);
        assert_installed(&directory);
        for (name, _) in SKILLS {
            let path = directory.join(name).join("SKILL.md");
            assert!(entries.iter().any(|entry| entry["agents"] == json!(agents)
                && entry["skill"] == name
                && entry["path"] == path.to_str().unwrap()));
        }
    }
    // Text output names each file once and no tool.
    let output = String::from_utf8(success(
        cli(home.path())
            .args(["skill", "install", "--force"])
            .output()
            .unwrap(),
    ))
    .unwrap();
    assert_eq!(output.lines().count(), 4);
    assert!(
        output
            .lines()
            .all(|line| line.starts_with("Installed skill "))
    );
    assert!(!retired.exists());
    assert_eq!(fs::read_dir(home.path()).unwrap().count(), 2);
}

#[test]
fn skill_install_refreshes_only_selected_tool_and_preserves_siblings() {
    let home = tempfile::tempdir().unwrap();
    let directory = home.path().join(".agents/skills/shoal-worker");
    fs::create_dir_all(&directory).unwrap();
    let source = home.path().join("source-skill.md");
    fs::write(&source, "personal source\n").unwrap();
    symlink(&source, directory.join("SKILL.md")).unwrap();
    fs::write(directory.join("notes.md"), "keep me\n").unwrap();
    // A skill from before Shoal recorded installations is Shoal's to replace.
    for args in [
        vec!["skill", "install", "codex"],
        vec!["skill", "install", "codex", "--force"],
    ] {
        success(cli(home.path()).args(&args).output().unwrap());
        assert_eq!(fs::read(directory.join("SKILL.md")).unwrap(), WORKER);
        assert_eq!(fs::read_to_string(&source).unwrap(), "personal source\n");
        assert_eq!(
            fs::read_to_string(directory.join("notes.md")).unwrap(),
            "keep me\n"
        );
        assert!(!home.path().join(".claude").exists());
        // An edit after installation survives until a forced install, without
        // leaving temporary files.
        fs::write(directory.join("SKILL.md"), "edited skill\n").unwrap();
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 2);
        let output = cli(home.path())
            .args(["skill", "install", "codex"])
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&success(output)).contains("Kept modified skill shoal-worker")
        );
        assert_eq!(
            fs::read_to_string(directory.join("SKILL.md")).unwrap(),
            "edited skill\n"
        );
    }
}

#[test]
fn unscoped_commands_update_installed_skills_after_upgrade() {
    let home = tempfile::tempdir().unwrap();
    let installed = home.path().join(".agents/skills");
    // An earlier version copied one skill before Shoal recorded installations.
    fs::create_dir_all(installed.join("shoal-worker")).unwrap();
    fs::write(installed.join("shoal-worker/SKILL.md"), "version one").unwrap();
    let run = |scope: Option<&str>, args: &[&str]| {
        let mut command = cli(home.path());
        command.env("SHOAL_STATE_DIR", home.path().join("state"));
        if let Some(token) = scope {
            command.env("SHOAL_SCOPE_TOKEN", token);
        }
        command.args(args).output().unwrap()
    };
    let output = run(Some("scoped"), &["shell", "completions", "bash"]);
    success(output);
    assert_eq!(
        fs::read_to_string(installed.join("shoal-worker/SKILL.md")).unwrap(),
        "version one"
    );
    // Printing a skill also updates the installed ones.
    let output = run(None, &["skill"]);
    assert!(String::from_utf8_lossy(&output.stderr).contains("Updated Shoal skills"));
    success(output);
    assert_installed(&installed);
    // Up to date skills and other tools' directories are left alone.
    let output = run(None, &["shell", "completions", "bash"]);
    assert!(output.stderr.is_empty(), "{output:?}");
    assert!(!home.path().join(".claude").exists());
}

#[test]
fn development_builds_leave_installed_skills_alone() {
    let home = tempfile::tempdir().unwrap();
    let installed = home.path().join(".agents/skills");
    fs::create_dir_all(installed.join("shoal-worker")).unwrap();
    fs::write(installed.join("shoal-worker/SKILL.md"), "version one").unwrap();
    let run = |args: &[&str]| {
        cli(home.path())
            .env_remove("SHOAL_DEV_USER_FILES")
            .args(args)
            .output()
            .unwrap()
    };
    // Printing a skill refreshes installed ones in a release build.
    success(run(&["skill"]));
    let install = run(&["skill", "install", "codex"]);
    assert!(!install.status.success());
    assert!(
        String::from_utf8_lossy(&install.stderr)
            .contains("development builds cannot install user-level skills")
    );
    let mut entries: Vec<_> = fs::read_dir(&installed)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    entries.sort();
    assert_eq!(entries, ["shoal-worker"]);
    assert_eq!(
        fs::read_to_string(installed.join("shoal-worker/SKILL.md")).unwrap(),
        "version one"
    );
    assert!(!home.path().join(".codex").exists());
}

#[test]
fn skill_install_honors_claude_override_and_rejects_invalid_paths_before_writes() {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("custom Claude config");
    success(
        cli(home.path())
            .env("CLAUDE_CONFIG_DIR", &config)
            .args(["skill", "install", "claude"])
            .output()
            .unwrap(),
    );
    assert_installed(&config.join("skills"));
    assert!(!home.path().join(".claude").exists());
    assert!(!home.path().join(".agents").exists());
    let invalid = cli(home.path())
        .env("CLAUDE_CONFIG_DIR", "relative")
        .args(["skill", "install"])
        .output()
        .unwrap();
    assert!(!invalid.status.success());
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("must be an absolute path"));
    assert!(!home.path().join(".agents").exists());
    let directory = config.join("skills/shoal-worker/SKILL.md");
    fs::remove_file(&directory).unwrap();
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("keep"), "keep").unwrap();
    let install = |force: bool| {
        let mut command = cli(home.path());
        command
            .env("CLAUDE_CONFIG_DIR", &config)
            .args(["skill", "install", "claude"]);
        if force {
            command.arg("--force");
        }
        command.output().unwrap()
    };
    success(install(false));
    assert!(!install(true).status.success());
    assert_eq!(fs::read_to_string(directory.join("keep")).unwrap(), "keep");
}

#[test]
fn scoped_processes_can_export_but_cannot_install_skills() {
    let home = tempfile::tempdir().unwrap();
    let output = cli(home.path())
        .env("SHOAL_SCOPE_TOKEN", "scoped")
        .args(["skill", "install"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot install user-level skills"));
    assert_eq!(
        success(
            cli(home.path())
                .env("SHOAL_SCOPE_TOKEN", "scoped")
                .arg("skill")
                .output()
                .unwrap()
        ),
        WORKER
    );
    assert_eq!(fs::read_dir(home.path()).unwrap().count(), 0);
}

#[test]
fn configured_ai_tools_install_selected_or_all_skills_and_override_defaults() {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("xdg/shoal");
    fs::create_dir_all(&config).unwrap();
    fs::write(
        config.join("config.toml"),
        format!(
            "[ai.pi]\nskill_dir = '~/pi skills'\n[ai.codex]\nskill_dir = {:?}\n",
            home.path().join("custom codex").to_str().unwrap()
        ),
    )
    .unwrap();
    let run = |agent: &str| {
        cli(home.path())
            .env("XDG_CONFIG_HOME", home.path().join("xdg"))
            .args(["--json", "skill", "install", agent])
            .output()
            .unwrap()
    };
    let installed: serde_json::Value = serde_json::from_slice(&success(run("pi"))).unwrap();
    assert_eq!(installed["installed"].as_array().unwrap().len(), 2);
    assert_eq!(installed["installed"][0]["agents"], json!(["pi"]));
    assert!(!home.path().join("custom codex").exists());
    fs::create_dir(home.path().join("custom codex")).unwrap();
    let installed: serde_json::Value = serde_json::from_slice(&success(run("all"))).unwrap();
    assert_eq!(installed["installed"].as_array().unwrap().len(), 4);
    for directory in ["pi skills", "custom codex"] {
        assert_installed(&home.path().join(directory));
    }
    assert!(!home.path().join(".agents").exists());
    assert!(!home.path().join(".claude").exists());
    let unknown = run("unknown");
    assert!(!unknown.status.success());
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("unknown AI tool"));
}

#[test]
fn invalid_ai_config_fails_before_installing_any_skill() {
    for invalid in [
        "[ai.pi]\nskill_dir = 'relative'",
        "[ai.pi]\nskill_dir = ''",
        "[ai.pi]\nskill_dir = '~someone/skills'",
        "[ai.pi]\nskill_dir = \"/tmp/\\u0000skills\"",
        "[ai.all]\nskill_dir = '~/skills'",
        "[ai.'bad name']\nskill_dir = '~/skills'",
        "[ai.pi]\nskill_directory = '~/skills'",
    ] {
        let home = tempfile::tempdir().unwrap();
        let config = home.path().join(".config/shoal");
        fs::create_dir_all(&config).unwrap();
        fs::write(config.join("config.toml"), invalid).unwrap();
        let result = cli(home.path())
            .args(["skill", "install"])
            .output()
            .unwrap();
        assert!(!result.status.success(), "{invalid}");
        assert!(!home.path().join(".agents").exists());
        assert!(!home.path().join(".claude").exists());
        // Export remains independent of configuration.
        success(cli(home.path()).arg("skill").output().unwrap());
    }
}
