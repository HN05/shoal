use crate::common;
use crate::support::Fixture;
use serde_json::Value;
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

#[test]
fn noninteractive_missing_targets_do_not_open_pickers() {
    let fixture = Fixture::new();
    for args in [&["add"][..], &["exec", "--", "true"], &["rm"]] {
        let output = fixture.run(args);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("non-interactive"));
    }
}

#[test]
fn shell_function_navigates_after_add_and_away_after_rm() {
    let fixture = Fixture::new();
    let integration = fixture.root.path().join("integration.sh");
    fs::write(&integration, fixture.run(&["shell", "init"]).stdout).unwrap();
    let script = r#"
set -e
. "$INTEGRATION"
cd "$REPO"
shoal add "$REPO" navigate
test "${PWD##*/}" = navigate
shoal_repo_dir="$(dirname "$PWD")"
shoal cd -
test "$PWD" = "$REPO"
shoal cd -
test "${PWD##*/}" = navigate
cd "$REPO"
shoal cd navigate
test "${PWD##*/}" = navigate
if shoal exec navigate -- sh -c 'exit 7'; then
  exit 1
else
  test "$?" -eq 7
fi
printf 'keep me' > untracked
if shoal rm; then
  exit 1
fi
test "${PWD##*/}" = navigate
test -f untracked
rm untracked
mkdir nested
cd nested
shoal rm
test "$PWD" = "$shoal_repo_dir"
if shoal cd -; then
  exit 1
fi
test "$PWD" = "$shoal_repo_dir"
shoal add "$REPO" acknowledged
test "${PWD##*/}" = acknowledged
printf 'retained' > untracked
shoal pr merged
test "${PWD##*/}" = acknowledged
shoal pr clear
rm untracked
shoal pr merged
test "$PWD" = "$shoal_repo_dir"
until ! shoal inspect acknowledged >/dev/null 2>&1; do sleep 0.1; done
printf 'navigation-ok\n'
"#;
    for shell in ["bash", "zsh"] {
        let output = common::isolated(shell)
            .arg("-c")
            .arg(script)
            .env("INTEGRATION", &integration)
            .env("REPO", &fixture.repo)
            .env("SHOAL_STATE_DIR", fixture.root.path().join("state"))
            .env("HOME", fixture.root.path())
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    Path::new(env!("CARGO_BIN_EXE_shoal"))
                        .parent()
                        .unwrap()
                        .display(),
                    std::env::var("PATH").unwrap()
                ),
            )
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{shell}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).ends_with("navigation-ok\n"));
    }
}

#[test]
fn interactive_navigation_reports_missing_shell_integration() {
    let fixture = Fixture::new();
    let workspace = fixture.add("existing");

    let (cd, cd_stderr) = fixture.interactive(&["cd", "existing"], "");
    assert!(cd.status.success());
    assert_eq!(
        String::from_utf8(cd.stdout).unwrap(),
        format!("{}\n", workspace["path"].as_str().unwrap())
    );
    assert!(cd_stderr.contains("shell integration is not loaded"));
    assert!(cd_stderr.contains("source <(shoal shell init)"));

    let (add, add_stderr) =
        fixture.interactive(&["add", fixture.repo.to_str().unwrap(), "created"], "");
    assert!(add.status.success());
    assert!(
        String::from_utf8(add.stdout)
            .unwrap()
            .contains("Created created")
    );
    assert!(add_stderr.contains("shell integration is not loaded"));
    assert!(add_stderr.contains("source <(shoal shell init)"));

    let piped = fixture.run(&["cd", "existing"]);
    assert!(piped.status.success());
    assert!(piped.stderr.is_empty());
    assert_eq!(
        String::from_utf8(piped.stdout).unwrap(),
        format!("{}\n", workspace["path"].as_str().unwrap())
    );

    let json = fixture.ok(&["cd", "existing"]);
    assert_eq!(json["path"], workspace["path"]);
}

#[test]
fn cd_previous_checks_existence_and_json_never_navigates() {
    let fixture = Fixture::new();
    let workspace = fixture.add("previous");
    let path = Path::new(workspace["path"].as_str().unwrap());
    let directive = fixture.root.path().join("cd-directive");
    fs::write(&directive, "").unwrap();
    let output = fixture
        .command()
        .env("OLDPWD", path)
        .env_remove("SHOAL_PREVIOUS_DIR")
        .env("SHOAL_SHELL_DIRECTIVE", &directive)
        .args(["--json", "cd", "-"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["path"],
        fs::canonicalize(path).unwrap().to_str().unwrap()
    );
    assert_eq!(fs::read_to_string(&directive).unwrap(), "");
    fixture.ok(&["rm", "previous"]);
    let deleted = fixture
        .command()
        .env("OLDPWD", path)
        .env_remove("SHOAL_PREVIOUS_DIR")
        .env("SHOAL_SHELL_DIRECTIVE", &directive)
        .args(["cd", "-"])
        .output()
        .unwrap();
    assert!(!deleted.status.success());
    assert!(String::from_utf8_lossy(&deleted.stderr).contains("no longer exists"));
    assert_eq!(fs::read_to_string(&directive).unwrap(), "");
    for previous in [None, Some("relative/directory")] {
        let mut command = fixture.command();
        command
            .env_remove("OLDPWD")
            .env_remove("SHOAL_PREVIOUS_DIR");
        if let Some(previous) = previous {
            command.env("OLDPWD", previous);
        }
        assert!(!command.args(["cd", "-"]).output().unwrap().status.success());
    }
}

#[test]
fn cd_previous_cannot_escape_execution_scope() {
    let fixture = Fixture::new();
    let first = fixture.add("first");
    let other = fixture.add("other");
    let binary = env!("CARGO_BIN_EXE_shoal");
    let scoped = |destination: &str| {
        fixture
            .command()
            .env("SHOAL_PREVIOUS_DIR", destination)
            .args(["exec", "first", "--", binary, "--json", "cd", "-"])
            .output()
            .unwrap()
    };
    assert!(scoped(first["path"].as_str().unwrap()).status.success());
    for path in [
        other["path"].as_str().unwrap(),
        fixture.repo.to_str().unwrap(),
    ] {
        let output = scoped(path);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("cannot navigate outside"));
    }
}

#[test]
fn menu_bindings_preserve_enter_inspect_cancel_and_scope() {
    let fixture = Fixture::new();
    let workspace = fixture.add("menu-worker");
    let bin = fixture.root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let picker = bin.join("fzf");
    for scoped in [false, true] {
        for key in ["", "ctrl-o", "ctrl-z", "cancel"] {
            fs::write(
                &picker,
                format!(
                    r#"#!/bin/sh
printf '%s\n' "$@" > "$HOME/picker-args"
cat > "$HOME/picker-input"
[ '{key}' != cancel ] || exit 130
printf '%s\n' '{key}'
head -n 1 "$HOME/picker-input"
"#
                ),
            )
            .unwrap();
            fs::set_permissions(&picker, fs::Permissions::from_mode(0o755)).unwrap();
            let args = if scoped {
                vec!["exec", "menu-worker", "--", env!("CARGO_BIN_EXE_shoal")]
            } else {
                vec![]
            };
            fs::remove_file(fixture.root.path().join("picker-input")).ok();
            let (output, transcript) = fixture.interactive(&args, "");
            assert!(
                fixture.root.path().join("picker-input").exists(),
                "picker did not run: {transcript}"
            );
            let args = fs::read_to_string(fixture.root.path().join("picker-args")).unwrap();
            let input = fs::read_to_string(fixture.root.path().join("picker-input")).unwrap();
            assert!(input.contains(workspace["id"].as_str().unwrap()));
            assert_eq!(input.contains("+ Add workspace"), !scoped);
            let (keys, header) = if scoped {
                (
                    "ctrl-e,ctrl-o,ctrl-f",
                    "enter: enter   ctrl-e: execute   ctrl-o: inspect   ctrl-f: diff",
                )
            } else {
                (
                    "ctrl-d,ctrl-e,ctrl-a,ctrl-o,ctrl-s,ctrl-f",
                    "enter: enter   ctrl-d: delete   ctrl-e: execute   ctrl-a: add   ctrl-o: inspect   ctrl-s: stop   ctrl-f: diff",
                )
            };
            assert!(args.contains(&format!("--expect={keys}\n--header\n{header}\n")));
            match key {
                "cancel" => {
                    assert!(!output.status.success());
                    assert!(transcript.contains("selection canceled"), "{transcript}");
                }
                "ctrl-z" => {
                    assert!(!output.status.success());
                    assert!(transcript.contains("unknown picker action"), "{transcript}");
                }
                _ => {
                    assert!(output.status.success(), "{transcript}");
                    assert!(String::from_utf8_lossy(&output.stdout).contains("menu-worker"));
                }
            }
        }
    }
}

#[test]
fn cd_always_picks_even_inside_a_workspace_and_cancel_does_not_navigate() {
    use std::os::fd::FromRawFd;
    let fixture = Fixture::new();
    let first = fixture.add("first");
    let second = fixture.add("second");
    let mut broken = fixture.command();
    let failed = broken
        .args([
            "add",
            fixture.repo.to_str().unwrap(),
            "missing",
            "--base",
            "not-a-ref",
        ])
        .output()
        .unwrap();
    assert!(!failed.status.success());
    let bin = fixture.root.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    let fzf = bin.join("fzf");
    fs::write(&fzf, "#!/bin/sh\ncat > \"$SHOAL_TEST_PICK_INPUT\"\n[ \"$SHOAL_TEST_PICK_ID\" != cancel ] || exit 130\nawk -F '\\t' -v id=\"$SHOAL_TEST_PICK_ID\" '$1 == id { print }' \"$SHOAL_TEST_PICK_INPUT\"\n").unwrap();
    fs::set_permissions(&fzf, fs::Permissions::from_mode(0o755)).unwrap();
    let directive = fixture.root.path().join("cd-directive");
    let input = fixture.root.path().join("picker-input");
    for choice in [second["id"].as_str().unwrap(), "cancel"] {
        fs::write(&directive, "").unwrap();
        let (mut master, mut slave) = (-1, -1);
        // Give the CLI real terminal handles so its normal interactive path runs.
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        let _master = unsafe { fs::File::from_raw_fd(master) };
        let slave = unsafe { fs::File::from_raw_fd(slave) };
        let output = fixture
            .command()
            .current_dir(first["path"].as_str().unwrap())
            .env("SHOAL_TEST_PICK_INPUT", &input)
            .env("SHOAL_TEST_PICK_ID", choice)
            .env("SHOAL_SHELL_DIRECTIVE", &directive)
            .arg("cd")
            .stdin(slave.try_clone().unwrap())
            .stderr(slave)
            .output()
            .unwrap();
        let choices = fs::read_to_string(&input).unwrap();
        assert!(choices.contains(first["id"].as_str().unwrap()));
        assert!(choices.contains(second["id"].as_str().unwrap()));
        assert!(!choices.contains("missing"));
        if choice == "cancel" {
            assert!(!output.status.success());
            assert_eq!(fs::read_to_string(&directive).unwrap(), "");
        } else {
            assert!(output.status.success());
            assert_eq!(
                fs::read_to_string(&directive).unwrap().trim(),
                second["path"].as_str().unwrap()
            );
        }
    }
    // JSON/piped invocations must not silently choose the current workspace.
    let output = fixture
        .command()
        .current_dir(first["path"].as_str().unwrap())
        .args(["--json", "cd"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("explicit target"));
}

#[test]
fn workspace_context_adapters_preserve_directory_scope_and_picker_policy() {
    let fixture = Fixture::new();
    let own = fixture.add("context-own");
    let other = fixture.add("context-other");
    let own_path = Path::new(own["path"].as_str().unwrap());
    let other_path = Path::new(other["path"].as_str().unwrap());
    for (path, name) in [(own_path, "own-only"), (other_path, "other-only")] {
        fs::write(
            path.join(".shoal.toml"),
            format!("[commands]\n{name} = ['true']\n"),
        )
        .unwrap();
    }
    let nested = own_path.join("nested/deep");
    fs::create_dir_all(&nested).unwrap();
    let alias = fixture.root.path().join("alias");
    std::os::unix::fs::symlink(&nested, &alias).unwrap();

    let command = |cwd: &Path, scoped: bool, completion: bool| {
        if scoped {
            let mut command = fixture.command();
            command
                .args([
                    "exec",
                    "context-own",
                    "--",
                    "sh",
                    "-c",
                    "cd \"$1\"; shift; exec \"$@\"",
                    "context-test",
                ])
                .arg(cwd)
                .arg("env");
            // Completion variables belong to the inner process, after scope delivery.
            if completion {
                command.args(["SHOAL_COMPLETE=bash", "_CLAP_COMPLETE_INDEX=2"]);
            }
            command.arg(env!("CARGO_BIN_EXE_shoal"));
            command
        } else {
            let mut command = fixture.command();
            command.current_dir(cwd);
            if completion {
                command
                    .env("SHOAL_COMPLETE", "bash")
                    .env("_CLAP_COMPLETE_INDEX", "2")
                    .env("SHOAL_STATE_DIR", fixture.root.path().join("state"));
            }
            command
        }
    };
    for (cwd, scoped) in [
        (nested.as_path(), false),
        (alias.as_path(), false),
        (fixture.root.path(), true),
        (other_path, true),
    ] {
        for args in [vec!["--json", "run"], vec!["--json", "config", "show"]] {
            let output = command(cwd, scoped, false).args(&args).output().unwrap();
            assert!(output.status.success(), "{args:?}: {output:?}");
            let text = String::from_utf8(output.stdout).unwrap();
            assert!(text.contains("own-only"), "{text}");
            assert!(!text.contains("other-only"), "{text}");
        }
        let output = command(cwd, scoped, false)
            .args(["--json", "status"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let status: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(status["workspace"]["id"], own["id"]);

        let output = command(cwd, scoped, true)
            .args(["--", "shoal", "run", "own"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line == "own-only")
        );
    }
    for args in [
        vec!["status", "context-other"],
        vec!["config", "show", "context-other"],
    ] {
        let output = command(other_path, true, false)
            .args(&args)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("scope"),
            "{output:?}"
        );
    }
    for args in [vec!["inspect"], vec!["cd"]] {
        let output = command(&nested, false, false).args(&args).output().unwrap();
        assert!(!output.status.success(), "{args:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("non-interactive"));
    }
    let output = command(fixture.root.path(), false, false)
        .args(["--json", "run"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("own-only"));
    let output = command(fixture.root.path(), false, false)
        .args(["config", "show"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("no current workspace or registered checkout")
    );
}
