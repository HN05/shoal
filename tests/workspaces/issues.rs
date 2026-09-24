use crate::support::{Fixture, git};
use serde_json::Value;
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

#[test]
fn add_from_issue_uses_existing_forge_cli_and_passes_context_to_agents() {
    let fixture = Fixture::with_config(Some("[codex]\ndefault_mode = 'app'\n"));
    let bin = fixture.root.path().join("issue-bin");
    fs::create_dir(&bin).unwrap();
    for tool in ["gh", "fj"] {
        let script = bin.join(tool);
        fs::write(&script, "#!/bin/sh\nprintf '%s\\0' \"$@\" > \"$ISSUE_ARGS\"\npwd > \"$ISSUE_CWD\"\ncat \"$ISSUE_RESPONSE\"\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    }
    for tool in ["codex", "claude"] {
        let script = bin.join(tool);
        fs::write(
            &script,
            "#!/bin/sh\nprintf '%s\\0' \"$@\" > \"$AGENT_ARGS\"\n",
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let response = fixture.root.path().join("issue-response");
    let issue_args = fixture.root.path().join("issue-args");
    let issue_cwd = fixture.root.path().join("issue-cwd");
    let agent_args = fixture.root.path().join("agent-args");
    let title = "Fix API timeout; $(false)";
    let body = "Reproduce with two clients.\nKeep the connection alive.";
    for (index, (host, agent, as_url)) in [
        ("github.com", "codex", false),
        ("github.com", "claude", true),
        ("forge.example", "codex", true),
        ("forge.example", "claude", false),
    ]
    .into_iter()
    .enumerate()
    {
        let number = index + 34;
        let url = format!("https://{host}/team/project/issues/{number}");
        let remote = format!("git@{host}:team/project.git");
        if index == 0 {
            git(&fixture.repo, &["remote", "add", "origin", &remote]);
        } else {
            git(&fixture.repo, &["remote", "set-url", "origin", &remote]);
        }
        let text = if host == "github.com" {
            serde_json::json!({"number": number, "title": title, "body": body}).to_string()
        } else {
            format!(
                "\u{2068}{title}\u{2069} #\u{2068}{number}\u{2069}\"\nBy user — Open\n\n> {body}\n\n0 comments\n"
            )
        };
        fs::write(&response, text).unwrap();
        let input = if as_url {
            url.clone()
        } else {
            number.to_string()
        };
        let output = fixture
            .command()
            .args([
                "--json",
                "add",
                fixture.repo.to_str().unwrap(),
                "--base",
                "HEAD",
                "--issue",
                &input,
                "--agent",
                agent,
                "--",
                "--model",
                "test-model",
            ])
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("ISSUE_RESPONSE", &response)
            .env("ISSUE_ARGS", &issue_args)
            .env("ISSUE_CWD", &issue_cwd)
            .env("AGENT_ARGS", &agent_args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let workspace: Value = serde_json::from_slice(&output.stdout).unwrap();
        let name = format!("issue-{number}-fix-api-timeout-false");
        assert_eq!(workspace["name"], name);
        assert_eq!(workspace["branch"], name);
        assert_eq!(
            fs::read_to_string(&issue_cwd).unwrap().trim(),
            fixture.repo.to_str().unwrap()
        );
        let invocation = fs::read_to_string(&issue_args).unwrap();
        assert!(invocation.contains(&format!("issue\0view\0{number}\0")));
        if host == "github.com" {
            assert!(invocation.contains("--repo\0github.com/team/project\0"));
        } else {
            assert!(invocation.contains("--host\0forge.example\0--remote\0origin\0"));
        }
        let invocation = fs::read_to_string(&agent_args).unwrap();
        let prompt = invocation.split('\0').next().unwrap();
        assert!(prompt.contains(title));
        assert!(prompt.contains(&url));
        assert!(prompt.contains(body));
        assert!(invocation.contains("\0--model\0test-model\0"));
        assert_eq!(
            fixture.ok(&["inspect", &name])["executions"],
            serde_json::json!([])
        );
    }
    // An explicit branch name still loads the issue, with no agent required.
    let output = fixture
        .command()
        .args([
            "--json",
            "add",
            fixture.repo.to_str().unwrap(),
            "custom-issue-name",
            "--base",
            "HEAD",
            "--issue",
            "37",
        ])
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("ISSUE_RESPONSE", &response)
        .env("ISSUE_ARGS", &issue_args)
        .env("ISSUE_CWD", &issue_cwd)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["name"],
        "custom-issue-name"
    );
}

#[test]
fn issue_templates_resolve_saved_config_then_worktree_then_global() {
    let fixture = Fixture::new();
    let config_dir = fixture.root.path().join(".config/shoal");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("issue-template.md"),
        "global {number}: {title} {body}",
    )
    .unwrap();
    let bin = fixture.root.path().join("template-bin");
    fs::create_dir(&bin).unwrap();
    for (tool, script) in [
        (
            "gh",
            r#"#!/bin/sh
printf '%s' '{"number":44,"title":"Literal {body}","body":"$(false)"}'
"#,
        ),
        ("claude", "#!/bin/sh\nprintf '%s' \"$1\"\n"),
    ] {
        let path = bin.join(tool);
        fs::write(&path, script).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fixture.add_github_origin();
    for (index, expected) in [
        "global 44: Literal {body} $(false)",
        "repo Literal {body}",
        "saved $(false)",
        "",
    ]
    .into_iter()
    .enumerate()
    {
        if index == 1 {
            fs::write(fixture.repo.join("issue-template.md"), "repo {title}").unwrap();
            git(&fixture.repo, &["add", "issue-template.md"]);
            git(
                &fixture.repo,
                &[
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.invalid",
                    "commit",
                    "-m",
                    "Add template",
                ],
            );
        }
        if index >= 2 {
            let local = fixture.root.path().join("local.toml");
            fs::write(
                &local,
                if index == 2 {
                    "issue_template = 'saved {body}'"
                } else {
                    "issue_template = ''"
                },
            )
            .unwrap();
            fixture.ok(&[
                "repo",
                "config",
                fixture.repo.to_str().unwrap(),
                "--file",
                local.to_str().unwrap(),
            ]);
        }
        let output = fixture
            .command()
            .args([
                "--json",
                "add",
                fixture.repo.to_str().unwrap(),
                &format!("template-{index}"),
                "--base",
                "HEAD",
                "--issue",
                "44",
                "--agent",
                "claude",
            ])
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert_eq!(stdout.split_once('\n').unwrap().1, expected);
    }
}

#[test]
fn issue_command_finds_the_repository_and_starts_the_default_agent() {
    let fixture = Fixture::with_config(Some("default_agent = 'claude'\n"));
    let other = fixture.root.path().join("other");
    git(
        &fixture.repo,
        &["clone", "-q", ".", other.to_str().unwrap()],
    );
    git(
        &other,
        &[
            "remote",
            "set-url",
            "origin",
            "git@github.com:team/other.git",
        ],
    );
    fixture.ok(&["repo", "add", other.to_str().unwrap()]);
    fixture.add_github_origin();
    let bin = fixture.root.path().join("issue-bin");
    fs::create_dir(&bin).unwrap();
    let gh = bin.join("gh");
    fs::write(
        &gh,
        "#!/bin/sh\nprintf '%s\\0' \"$@\" > \"$ISSUE_ARGS\"\ncat \"$ISSUE_RESPONSE\"\n",
    )
    .unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o700)).unwrap();
    for tool in ["codex", "claude"] {
        let script = bin.join(tool);
        fs::write(
            &script,
            "#!/bin/sh\nprintf '%s\\0' \"$(basename \"$0\")\" \"$@\" > \"$AGENT_ARGS\"\n",
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let response = fixture.root.path().join("issue-response");
    let issue_args = fixture.root.path().join("issue-args");
    let agent_args = fixture.root.path().join("agent-args");
    let body = "Paste the URL and go.";
    for (number, agent, expected) in [
        (41, None, "claude"),
        (42, Some("codex"), "codex"),
        (43, None, "codex"),
        (45, None, "codex"),
        (46, None, "codex"),
        (47, None, "codex"),
    ] {
        if number == 43 {
            // The repository's checked-in default wins over the global one.
            fs::write(
                fixture.repo.join(".shoal.toml"),
                "default_agent = 'codex'\n",
            )
            .unwrap();
            git(&fixture.repo, &["add", ".shoal.toml"]);
            git(
                &fixture.repo,
                &[
                    "-c",
                    "user.name=Shoal Test",
                    "-c",
                    "user.email=shoal@example.invalid",
                    "commit",
                    "-q",
                    "-m",
                    "default agent",
                ],
            );
        }
        fs::write(
            &response,
            serde_json::json!({"number": number, "title": "Paste an issue", "body": body})
                .to_string(),
        )
        .unwrap();
        let url = format!("https://github.com/team/project/issues/{number}");
        let number_input = number.to_string();
        let input = if number >= 45 { &number_input } else { &url };
        let mut args = vec!["--json", "issue", input, "--base", "HEAD"];
        if number == 47 {
            args.extend(["--repo", fixture.repo.to_str().unwrap()]);
        }
        if let Some(agent) = agent {
            args.extend(["--agent", agent]);
        }
        args.extend(["--", "--model", "test-model"]);
        let cwd = match number {
            45 => fixture.repo.join("nested"),
            46 => PathBuf::from(
                fixture.ok(&["inspect", "issue-41-paste-an-issue"])["workspace"]["path"]
                    .as_str()
                    .unwrap(),
            )
            .join("nested"),
            _ => other.clone(),
        };
        fs::create_dir_all(&cwd).unwrap();
        let output = fixture
            .command()
            .args(&args)
            .current_dir(cwd)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("ISSUE_RESPONSE", &response)
            .env("ISSUE_ARGS", &issue_args)
            .env("AGENT_ARGS", &agent_args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let workspace: Value = serde_json::from_slice(&output.stdout).unwrap();
        let name = format!("issue-{number}-paste-an-issue");
        assert_eq!(workspace["name"], name);
        assert_eq!(workspace["branch"], name);
        assert!(fs::read_to_string(&issue_args).unwrap().contains(&format!(
            "issue\0view\0{number}\0--repo\0github.com/team/project\0"
        )));
        let invocation = fs::read_to_string(&agent_args).unwrap();
        let mut parts = invocation.split('\0');
        assert_eq!(parts.next(), Some(expected));
        let prompt = parts.next().unwrap();
        assert!(prompt.contains(&url) && prompt.contains(body), "{prompt}");
        assert!(invocation.contains("\0--model\0test-model\0"));
        assert_eq!(
            fixture.ok(&["inspect", &name])["executions"],
            serde_json::json!([])
        );
    }
    // `add` accepts numbers and URLs without applying either configured default agent.
    fs::remove_file(&agent_args).unwrap();
    for (number, repository, input) in [
        (44, None, "https://github.com/team/project/issues/44"),
        (68, Some(fixture.repo.to_str().unwrap()), "68"),
    ] {
        fs::write(
            &response,
            serde_json::json!({"number": number, "title": "Create workspace", "body": body})
                .to_string(),
        )
        .unwrap();
        let mut command = fixture.command();
        command.args(["--json", "add"]);
        if let Some(repository) = repository {
            command.arg(repository);
        }
        let output = command
            .args(["--issue", input, "--base", "HEAD"])
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("ISSUE_RESPONSE", &response)
            .env("ISSUE_ARGS", &issue_args)
            .env("AGENT_ARGS", &agent_args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let workspace: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            workspace["name"],
            format!("issue-{number}-create-workspace")
        );
        let inspection = fixture.ok(&["inspect", workspace["name"].as_str().unwrap()]);
        assert_eq!(inspection["workspace"]["state"], "ready");
        assert_eq!(inspection["executions"], serde_json::json!([]));
        assert!(!agent_args.exists());
    }

    // The default agent applies to the issue command, not ordinary additions.
    let output = fixture
        .command()
        .args([
            "--json",
            "add",
            fixture.repo.to_str().unwrap(),
            "plain",
            "--base",
            "HEAD",
        ])
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("AGENT_ARGS", &agent_args)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(!agent_args.exists());
}

#[test]
fn issue_number_picks_a_repository_before_lookup_interactively() {
    let fixture = Fixture::with_config(Some("default_agent = 'claude'\n"));
    fixture.add_github_origin();
    let bin = fixture.root.path().join("bin");
    fs::create_dir(&bin).unwrap();
    for (tool, script) in [
        ("fzf", "#!/bin/sh\nhead -n 1\n"),
        (
            "gh",
            "#!/bin/sh\nprintf 'lookup in %s' \"$PWD\" >&2\nexit 1\n",
        ),
    ] {
        let path = bin.join(tool);
        fs::write(&path, script).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let (output, transcript) = fixture.interactive(&["issue", "103", "--base", "HEAD"], "");
    assert!(!output.status.success(), "{output:?}\n{transcript}");
    assert!(
        transcript.contains(&format!("lookup in {}", fixture.repo.display())),
        "{transcript}"
    );
    assert_eq!(fixture.ok(&["list"]), serde_json::json!([]));
}

#[test]
fn issue_lookup_errors_never_create_a_workspace() {
    let fixture = Fixture::new();
    git(
        &fixture.repo,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/team/project.git",
        ],
    );
    let bin = fixture.root.path().join("issue-bin");
    fs::create_dir(&bin).unwrap();
    // Keep the missing-tool case independent of any installed forge clients.
    let git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|dir| dir.join("git"))
        .find(|path| path.is_file() && path.metadata().unwrap().permissions().mode() & 0o111 != 0)
        .unwrap()
        .canonicalize()
        .unwrap();
    std::os::unix::fs::symlink(git, bin.join("git")).unwrap();
    let tool = bin.join("gh");
    for (input, script, diagnostic) in [
        ("4", None, "install it"),
        (
            "4",
            Some("#!/bin/sh\necho login-required >&2\nexit 1\n"),
            "login-required",
        ),
        (
            "4",
            Some("#!/bin/sh\necho bad-json\n"),
            "invalid gh issue response",
        ),
        (
            "https://github.com/other/project/issues/4",
            None,
            "different repository",
        ),
    ] {
        if let Some(script) = script {
            fs::write(&tool, script).unwrap();
            fs::set_permissions(&tool, fs::Permissions::from_mode(0o700)).unwrap();
        } else if tool.exists() {
            fs::remove_file(&tool).unwrap();
        }
        let output = fixture
            .command()
            .args([
                "add",
                fixture.repo.to_str().unwrap(),
                "--base",
                "HEAD",
                "--issue",
                input,
            ])
            .env("PATH", &bin)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(diagnostic),
            "{output:?}"
        );
        assert_eq!(fixture.ok(&["list"]), serde_json::json!([]));
    }
    for (args, diagnostic) in [
        (
            vec!["add", "--issue", "https://github.com/team/other/issues/4"],
            "shoal repo add",
        ),
        (vec!["add", "--issue", "4"], "missing argument"),
        (
            vec!["issue", "https://github.com/team/project/issues/4"],
            "no agent selected",
        ),
        (vec!["issue", "4", "--agent", "codex"], "pass --repo"),
        (
            vec!["issue", "not-an-issue", "--agent", "codex"],
            "expected an issue URL",
        ),
        (
            vec![
                "issue",
                "0",
                "--repo",
                fixture.repo.to_str().unwrap(),
                "--agent",
                "codex",
            ],
            "must be positive",
        ),
        (
            vec!["issue", "4", "--repo", "unknown", "--agent", "codex"],
            "not registered",
        ),
        (
            vec![
                "issue",
                "https://github.com/team/other/issues/4",
                "--repo",
                fixture.repo.to_str().unwrap(),
                "--agent",
                "codex",
            ],
            "different repository",
        ),
        (
            vec![
                "issue",
                "https://github.com/team/other/issues/4",
                "--agent",
                "codex",
            ],
            "shoal repo add",
        ),
        (
            vec![
                "issue",
                "https://github.com/team/project/pull/4",
                "--agent",
                "codex",
            ],
            "/issues/<number>",
        ),
    ] {
        let output = fixture
            .command()
            .args(&args)
            .env("PATH", &bin)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(diagnostic),
            "{output:?}"
        );
        assert_eq!(fixture.ok(&["list"]), serde_json::json!([]));
    }
}
