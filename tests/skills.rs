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
fn packaged_binary_runs_from_deleted_cwd_and_installs_stable_skill_links() {
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
    let installed = home.path().join(".agents/skills");
    for (name, _) in SKILLS {
        assert_eq!(
            fs::read_link(installed.join(name).join("SKILL.md")).unwrap(),
            stable.join("skills").join(name).join("SKILL.md")
        );
    }
    let upgraded = home.path().join("version two");
    write_skills(&upgraded.join("skills"), "version two");
    fs::remove_file(&stable).unwrap();
    symlink(&upgraded, &stable).unwrap();
    for (name, _) in SKILLS {
        assert_eq!(
            fs::read_to_string(installed.join(name).join("SKILL.md")).unwrap(),
            "version two"
        );
    }

    // The explicit runtime source still overrides the adjacent package link.
    success(
        support::isolated(home.path(), bin.join("shoal"))
            .env("SHOAL_SKILLS_DIR", package.join("skills"))
            .args(["skill", "install", "codex"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        fs::read_link(installed.join("shoal-worker/SKILL.md")).unwrap(),
        package.join("skills/shoal-worker/SKILL.md")
    );
    assert!(!home.path().join("state").exists());
}

#[test]
fn relative_packaged_skill_link_follows_homebrew_upgrades() {
    let home = tempfile::tempdir().unwrap();
    let prefix = fs::canonicalize(home.path()).unwrap();
    let old = prefix.join("Cellar/shoal/0.5.0");
    let new = prefix.join("Cellar/shoal/0.5.1");
    let skills = Path::new("share/shoal/skills");
    for (package, contents) in [(&old, "version one"), (&new, "version two")] {
        write_skills(&package.join(skills), contents);
    }
    fs::create_dir_all(old.join("libexec")).unwrap();
    copy_executable(home.path(), &old.join("libexec/shoal"));
    symlink(
        "../../../../opt/shoal/share/shoal/skills",
        old.join("libexec/shoal-skills"),
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
    let installed = home.path().join(".agents/skills/shoal-worker/SKILL.md");
    assert_eq!(
        fs::read_link(&installed).unwrap(),
        stable.join(skills).join("shoal-worker/SKILL.md")
    );
    assert_eq!(fs::read_to_string(&installed).unwrap(), "version one");
    fs::remove_file(&stable).unwrap();
    symlink("../Cellar/shoal/0.5.1", &stable).unwrap();
    fs::remove_dir_all(old).unwrap();
    assert_eq!(fs::read_to_string(&installed).unwrap(), "version two");
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
    // opencode and pi share Codex's directory; Grok's does not exist.
    assert_eq!(entries.len(), 8);
    for (agent, relative) in [
        ("codex", ".agents/skills"),
        ("opencode", ".agents/skills"),
        ("pi", ".agents/skills"),
        ("claude", ".claude/skills"),
    ] {
        let directory = home.path().join(relative);
        assert_installed(&directory);
        for (name, _) in SKILLS {
            let path = directory.join(name).join("SKILL.md");
            assert!(entries.iter().any(|entry| entry["agent"] == agent
                && entry["skill"] == name
                && entry["path"] == path.to_str().unwrap()));
        }
    }
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
    for _ in 0..2 {
        success(
            cli(home.path())
                .args(["skill", "install", "codex"])
                .output()
                .unwrap(),
        );
        assert_eq!(fs::read(directory.join("SKILL.md")).unwrap(), WORKER);
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
    assert_eq!(installed["installed"][0]["agent"], "pi");
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
