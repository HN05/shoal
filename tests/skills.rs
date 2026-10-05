mod support;

use std::{
    fs,
    os::unix::fs::symlink,
    path::Path,
    process::{Command, Output},
};

fn cli(home: &Path) -> Command {
    let mut command = support::cli(home);
    command
        .env("XDG_CONFIG_HOME", home.join(".config"))
        // Skill delivery must not depend on socket path length or daemon setup.
        .env("SHOAL_STATE_DIR", home.join("x".repeat(150)));
    command
}

fn success(output: Output) -> Vec<u8> {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[test]
fn packaged_binary_runs_from_deleted_cwd_and_installs_a_stable_skill_link() {
    let home = tempfile::tempdir_in("/tmp").unwrap();
    let package = home.path().join("version one");
    let stable = home.path().join("opt");
    let bin = home.path().join("bin");
    fs::create_dir_all(package.join("libexec")).unwrap();
    fs::create_dir(&bin).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_shoal"), package.join("libexec/shoal")).unwrap();
    fs::write(package.join("SKILL.md"), "version one").unwrap();
    symlink(&package, &stable).unwrap();
    symlink(stable.join("SKILL.md"), package.join("libexec/shoal-skill")).unwrap();
    symlink(package.join("libexec/shoal"), bin.join("shoal")).unwrap();

    for args in [
        vec!["--json", "doctor", "--all"],
        vec!["skill", "install", "codex"],
    ] {
        let deleted = home.path().join("deleted");
        fs::create_dir(&deleted).unwrap();
        let output = support::isolated(home.path(), "bash")
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
        if args.contains(&"doctor") {
            assert_eq!(output.status.code(), Some(2));
            let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(report["checks"][0]["name"], "daemon");
        } else {
            success(output);
        }
    }
    let installed = home.path().join(".agents/skills/shoal/SKILL.md");
    assert_eq!(fs::read_link(&installed).unwrap(), stable.join("SKILL.md"));
    let upgraded = home.path().join("version two");
    fs::create_dir(&upgraded).unwrap();
    fs::write(upgraded.join("SKILL.md"), "version two").unwrap();
    fs::remove_file(&stable).unwrap();
    symlink(&upgraded, &stable).unwrap();
    assert_eq!(fs::read_to_string(&installed).unwrap(), "version two");

    // The explicit runtime source still overrides the adjacent package link.
    success(
        support::isolated(home.path(), bin.join("shoal"))
            .env("SHOAL_SKILL_PATH", package.join("SKILL.md"))
            .args(["skill", "install", "codex"])
            .output()
            .unwrap(),
    );
    assert_eq!(fs::read_link(installed).unwrap(), package.join("SKILL.md"));
    assert!(!home.path().join("state").exists());
}

#[test]
fn relative_packaged_skill_link_follows_homebrew_upgrades() {
    let home = tempfile::tempdir().unwrap();
    let prefix = fs::canonicalize(home.path()).unwrap();
    let old = prefix.join("Cellar/shoal/0.5.0");
    let new = prefix.join("Cellar/shoal/0.5.1");
    let skill = Path::new("share/shoal/skill/SKILL.md");
    for (package, contents) in [(&old, "version one"), (&new, "version two")] {
        fs::create_dir_all(package.join("share/shoal/skill")).unwrap();
        fs::write(package.join(skill), contents).unwrap();
    }
    fs::create_dir_all(old.join("libexec")).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_shoal"), old.join("libexec/shoal")).unwrap();
    symlink(
        "../../../../opt/shoal/share/shoal/skill/SKILL.md",
        old.join("libexec/shoal-skill"),
    )
    .unwrap();
    fs::create_dir(prefix.join("opt")).unwrap();
    let stable = prefix.join("opt/shoal");
    symlink("../Cellar/shoal/0.5.0", &stable).unwrap();
    fs::create_dir(prefix.join("bin")).unwrap();
    let launcher = prefix.join("bin/shoal");
    symlink("../Cellar/shoal/0.5.0/libexec/shoal", &launcher).unwrap();

    success(
        support::isolated(home.path(), &launcher)
            .args(["skill", "install", "codex"])
            .output()
            .unwrap(),
    );
    let installed = home.path().join(".agents/skills/shoal/SKILL.md");
    assert_eq!(fs::read_link(&installed).unwrap(), stable.join(skill));
    assert_eq!(fs::read_to_string(&installed).unwrap(), "version one");
    fs::remove_file(&stable).unwrap();
    symlink("../Cellar/shoal/0.5.1", &stable).unwrap();
    fs::remove_dir_all(old).unwrap();
    assert_eq!(fs::read_to_string(&installed).unwrap(), "version two");
}

#[test]
fn skill_export_and_default_install_work_without_daemon_or_repository() {
    let home = tempfile::tempdir().unwrap();
    let expected = include_bytes!("../SKILL.md");
    assert_eq!(
        success(cli(home.path()).arg("skill").output().unwrap()),
        expected
    );
    let exported: serde_json::Value = serde_json::from_slice(&success(
        cli(home.path()).args(["--json", "skill"]).output().unwrap(),
    ))
    .unwrap();
    assert_eq!(exported["skill"].as_str().unwrap().as_bytes(), expected);
    assert_eq!(fs::read_dir(home.path()).unwrap().count(), 0);
    let installed: serde_json::Value = serde_json::from_slice(&success(
        cli(home.path())
            .args(["--json", "skill", "install"])
            .output()
            .unwrap(),
    ))
    .unwrap();
    let entries = installed["installed"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    for (agent, relative) in [
        ("codex", ".agents/skills/shoal/SKILL.md"),
        ("claude", ".claude/skills/shoal/SKILL.md"),
    ] {
        let path = home.path().join(relative);
        assert_eq!(fs::read(&path).unwrap(), expected);
        assert!(
            entries
                .iter()
                .any(|entry| entry["agent"] == agent && entry["path"] == path.to_str().unwrap())
        );
    }
    assert_eq!(fs::read_dir(home.path()).unwrap().count(), 2);
}

#[test]
fn skill_install_refreshes_only_selected_skill_and_preserves_siblings() {
    let home = tempfile::tempdir().unwrap();
    let directory = home.path().join(".agents/skills/shoal");
    fs::create_dir_all(&directory).unwrap();
    let source = home.path().join("source-skill.md");
    fs::write(&source, "personal source\n").unwrap();
    symlink(&source, directory.join("SKILL.md")).unwrap();
    fs::write(directory.join("notes.md"), "keep me\n").unwrap();
    for _ in 0..2 {
        success(
            cli(home.path())
                .args(["skill", "install", "codex"])
                .output()
                .unwrap(),
        );
        assert_eq!(
            fs::read(directory.join("SKILL.md")).unwrap(),
            include_bytes!("../SKILL.md")
        );
        assert_eq!(fs::read_to_string(&source).unwrap(), "personal source\n");
        assert_eq!(
            fs::read_to_string(directory.join("notes.md")).unwrap(),
            "keep me\n"
        );
        assert!(!home.path().join(".claude").exists());
        // Refresh old contents on the next call, without leaving temporary files.
        fs::write(directory.join("SKILL.md"), "old installed skill\n").unwrap();
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 2);
    }
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
    assert_eq!(
        fs::read(config.join("skills/shoal/SKILL.md")).unwrap(),
        include_bytes!("../SKILL.md")
    );
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
    let directory = config.join("skills/shoal/SKILL.md");
    fs::remove_file(&directory).unwrap();
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("keep"), "keep").unwrap();
    assert!(
        !cli(home.path())
            .env("CLAUDE_CONFIG_DIR", &config)
            .args(["skill", "install", "claude"])
            .output()
            .unwrap()
            .status
            .success()
    );
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
        include_bytes!("../SKILL.md")
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
    assert_eq!(installed["installed"].as_array().unwrap().len(), 1);
    assert_eq!(installed["installed"][0]["agent"], "pi");
    assert!(!home.path().join("custom codex").exists());
    let installed: serde_json::Value = serde_json::from_slice(&success(run("all"))).unwrap();
    assert_eq!(installed["installed"].as_array().unwrap().len(), 3);
    for directory in ["pi skills", "custom codex", ".claude/skills"] {
        assert_eq!(
            fs::read(home.path().join(directory).join("shoal/SKILL.md")).unwrap(),
            include_bytes!("../SKILL.md")
        );
    }
    assert!(!home.path().join(".agents").exists());
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
