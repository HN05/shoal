//! Configured commands: `shoal run`, `shoal edit` and the commands each
//! config layer names.
use super::*;

#[test]
fn configured_commands_preserve_arguments_scope_and_exit_status() {
    let fixture = Fixture::with_config(Some(
        "[commands]\ncheck = ['sh', '-c', 'cat; printf \"%s\\n\" \"$SHOAL_WORKSPACE\" \"$@\"; test -n \"$SHOAL_SCOPE_TOKEN\" || exit 99; exit 7', 'check', 'literal $HOME']\n",
    ));
    let workspace = fixture.ok(&[
        "start",
        fixture.repo.to_str().unwrap(),
        "--branch",
        "custom",
    ]);
    let path = workspace["path"].as_str().unwrap();
    let mut child = fixture
        .command()
        .current_dir(path)
        .args(["run", "check", "--", "two words", "--flag"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"pipe:").unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(7),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout,
        b"pipe:custom\nliteral $HOME\ntwo words\n--flag\n"
    );
    assert_eq!(
        fixture.ok(&["status", &fixture.id("custom")])["executions"],
        serde_json::json!([])
    );
    fs::write(
        Path::new(path).join(".shoal.toml"),
        "[commands]\ncheck = ['printf', '%s', 'from worktree']\n",
    )
    .unwrap();
    assert_eq!(
        fixture
            .run(&["run", "check", "-w", &fixture.id("custom")])
            .stdout,
        b"from worktree"
    );
    let saved = fixture.root.path().join("commands.toml");
    fs::write(&saved, "[commands]\ncheck = ['printf', '%s', 'saved']\n").unwrap();
    fixture.save_local_config(fixture.repo.to_str().unwrap(), &saved);
    assert_eq!(
        fixture
            .run(&["run", "check", "-w", &fixture.id("custom")])
            .stdout,
        b"saved"
    );
}

#[test]
fn edit_runs_the_editor_or_the_configured_edit_command() {
    let fixture = Fixture::new();
    let workspace = fixture.add("editing");
    let path = workspace["path"].as_str().unwrap();
    let edit = |visual: Option<&str>, editor: Option<&str>| {
        let mut command = fixture.command();
        for (name, value) in [("VISUAL", visual), ("EDITOR", editor)] {
            match value {
                Some(value) => command.env(name, value),
                None => command.env_remove(name),
            };
        }
        command
            .args(["edit", &fixture.id("editing"), "--", "--flag"])
            .output()
            .unwrap()
    };
    let output = edit(Some("printf %s|"), Some("false"));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("{path}|--flag|")
    );
    assert_eq!(
        edit(None, Some("printf %s|")).stdout,
        format!("{path}|--flag|").as_bytes()
    );
    let output = edit(None, None);
    assert!(!output.status.success());

    fs::write(
        Path::new(path).join(".shoal.toml"),
        "[commands]\nedit = ['printf', '%s|', '{branch}']\n",
    )
    .unwrap();
    assert_eq!(edit(Some("false"), None).stdout, b"editing|--flag|");
}

#[test]
fn run_lists_command_layers_and_executes_names_that_collide_with_built_ins() {
    let fixture = Fixture::with_config(Some(
        "[commands]\nglobal = ['printf', '%s', 'global command']\nshadowed = ['global']\nlist = ['printf', '%s', 'configured list']\n[agents.claude]\ncommand = ['global-claude']\n",
    ));
    let workspace = fixture.add("run-command");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(
        path.join(".shoal.toml"),
        "[commands]\nshadowed = ['worktree']\nworktree = ['worktree-only']\n",
    )
    .unwrap();
    let saved = fixture.root.path().join("run-commands.toml");
    fs::write(
        &saved,
        "[commands]\nshadowed = ['saved']\nsaved = ['saved-only']\n",
    )
    .unwrap();
    fixture.save_local_config(fixture.repo.to_str().unwrap(), &saved);

    let output = fixture.run(&["run", "list", "--workspace", &fixture.id("run-command")]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"configured list");
    assert!(fixture.ok(&["list"]).is_array());

    let output = fixture
        .command()
        .current_dir(path)
        .args(["--json", "run"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let listed: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    let command = |name: &str| listed.iter().find(|entry| entry["name"] == name).unwrap();
    // Agents launch under [agents], so `run` lists only the shortcuts.
    assert!(!listed.iter().any(|entry| entry["name"] == "claude"));
    assert_eq!(command("global")["layer"], "global_config");
    assert_eq!(command("worktree")["layer"], "worktree_file");
    assert_eq!(command("saved")["layer"], "local_config");
    assert_eq!(command("shadowed")["argv"], serde_json::json!(["saved"]));
    assert_eq!(command("shadowed")["layer"], "local_config");

    let other = fixture.add("other-commands");
    let other_path = Path::new(other["path"].as_str().unwrap());
    fs::write(
        other_path.join(".shoal.toml"),
        "[commands]\nother = ['other-worktree-only']\n",
    )
    .unwrap();
    let output = fixture
        .command()
        .current_dir(path)
        .args([
            "--json",
            "run",
            "--workspace",
            &fixture.id("other-commands"),
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let listed: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert!(listed.iter().any(|entry| entry["name"] == "other"));
    assert!(!listed.iter().any(|entry| entry["name"] == "worktree"));
    assert!(
        !fixture
            .run(&["run", "--workspace", &fixture.id("missing-workspace")])
            .status
            .success()
    );
}

#[test]
fn repository_commands_pick_a_workspace_that_defines_them() {
    let fixture = Fixture::new();
    let defining = fixture.add("defining");
    fixture.add("other");
    let path = Path::new(defining["path"].as_str().unwrap());
    fs::write(
        path.join(".shoal.toml"),
        "[commands]\nlocal-check = ['printf', '%s', 'repository command']\n",
    )
    .unwrap();
    let id = defining["id"].as_str().unwrap();
    let (output, rows) = fixture.pick(&["run", "local-check"], id, "");
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"repository command");
    assert!(rows.starts_with(&format!("{id}\t")), "{rows:?}");
    assert_eq!(rows.lines().count(), 1, "{rows:?}");
    let previous = fixture.run(&["run", "local-check", "-w", "-"]);
    assert!(previous.status.success(), "{previous:?}");
    assert_eq!(previous.stdout, b"repository command");
    let (output, rows) = fixture.pick(&["run", "lsit"], id, "");
    assert!(!output.status.success());
    assert!(rows.is_empty(), "{rows:?}");
}

/// The error message of a failed run, read from JSON when `json` is set.
fn failure_message(output: &Output, json: bool) -> String {
    if !json {
        return String::from_utf8_lossy(&output.stderr).into_owned();
    }
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    error["error"]["message"].as_str().unwrap().to_owned()
}

#[test]
fn undefined_commands_fail_as_unknown_in_every_form() {
    let fixture = Fixture::new();
    let workspace = fixture.add("command-target");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(
        path.join(".shoal.toml"),
        "[commands]\nlocal-check = ['printf', '%s', 'repository command']\n",
    )
    .unwrap();
    for directory in [fixture.root.path(), path] {
        for (format, json) in [("--text", false), ("--json", true)] {
            for args in [
                vec!["run", "bogus"],
                vec!["run", "bogus", "--", "x"],
                vec!["run", "bogus", "--workspace", &fixture.id("command-target")],
            ] {
                let output = fixture
                    .command()
                    .current_dir(directory)
                    .arg(format)
                    .args(&args)
                    .output()
                    .unwrap();
                assert!(!output.status.success(), "{directory:?} {args:?}");
                assert!(output.stdout.is_empty(), "{args:?}: {output:?}");
                let error = failure_message(&output, json);
                assert!(error.contains("unknown command \"bogus\""), "{error}");
                assert!(!error.contains("workspace"), "{args:?}: {error}");
            }
        }
    }
    let output = fixture.run(&[
        "run",
        "local-check",
        "-w",
        &fixture.id("command-target"),
        "--",
        "extra",
    ]);
    assert_eq!(output.stdout, b"repository commandextra", "{output:?}");
}

#[test]
fn a_top_level_name_runs_no_configured_command_or_agent() {
    let fixture = Fixture::with_config(Some(
        "[commands]
check = ['printf', '%s', 'ran']
",
    ));
    let workspace = fixture.add("command-target");
    let path = Path::new(workspace["path"].as_str().unwrap());
    for args in [
        vec!["check"],
        vec!["check", &fixture.id("command-target")],
        vec!["claude", &fixture.id("command-target")],
        vec!["bogus", "--", "x"],
    ] {
        let output = fixture
            .command()
            .current_dir(path)
            .args(&args)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
        assert!(output.stdout.is_empty(), "{args:?}: {output:?}");
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains(&format!("'{}'", args[0])), "{error}");
        assert!(error.contains("`shoal run <name>`"), "{error}");
    }
}

#[test]
fn misspelled_built_ins_are_rejected_with_a_suggestion() {
    let fixture = Fixture::new();
    let workspace = fixture.add("command-target");
    let path = Path::new(workspace["path"].as_str().unwrap());
    for directory in [fixture.root.path(), path] {
        for args in [vec!["lease"], vec!["lease", &fixture.id("command-target")]] {
            let output = fixture
                .command()
                .current_dir(directory)
                .args(&args)
                .output()
                .unwrap();
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success(), "{directory:?} {args:?}");
            assert!(error.contains("'leases'"), "{error}");
        }
    }
    // A similar built-in name must not prevent an explicitly configured shortcut.
    fs::write(
        path.join(".shoal.toml"),
        "[commands]\nports = ['printf', '%s', 'configured command']\n",
    )
    .unwrap();
    let output = fixture.run(&["run", "ports", "-w", &fixture.id("command-target")]);
    assert!(output.status.success());
    assert_eq!(output.stdout, b"configured command");
}
