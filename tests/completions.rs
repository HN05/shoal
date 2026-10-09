mod support;

use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

/// Agents complete only when their executables are installed.
fn install_programs(home: &Path, programs: &[&str]) {
    let bin = home.join("bin");
    fs::create_dir_all(&bin).unwrap();
    for program in programs {
        let path = bin.join(program);
        fs::write(&path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn completion(home: &Path, shell: &str, words: &[&str]) -> Command {
    let mut command = support::cli(home);
    command
        .arg("--")
        .args(words)
        .env("SHOAL_COMPLETE", shell)
        .env("_CLAP_COMPLETE_INDEX", (words.len() - 1).to_string());
    command
}

/// Candidate values, without the descriptions zsh appends after a colon.
fn candidates(mut command: Command) -> Vec<String> {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| line.split(':').next().unwrap().to_owned())
        .collect()
}

fn generate(home: &Path, args: &[&str]) -> Vec<u8> {
    let output = support::cli(home).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[test]
fn bash_completes_commands_and_flags_without_a_daemon() {
    let home = tempfile::tempdir().unwrap();
    install_programs(home.path(), &["codex"]);
    let script = home.path().join("completions.bash");
    fs::write(&script, generate(home.path(), &["completions", "bash"])).unwrap();
    let output = support::isolated(home.path(), "bash").args(["--noprofile", "--norc", "-c", r#"
source "$1"
COMP_TYPE=9
COMP_WORDS=(shoal co); COMP_CWORD=1; COMP_LINE='shoal co'; COMP_POINT=${#COMP_LINE}
_clap_complete_shoal "${COMP_WORDS[0]}" "${COMP_WORDS[COMP_CWORD]}" "${COMP_WORDS[COMP_CWORD-1]}"; printf '%s\n' "${COMPREPLY[@]}"
COMP_WORDS=(shoal codex --c); COMP_CWORD=2; COMP_LINE='shoal codex --c'; COMP_POINT=${#COMP_LINE}
_clap_complete_shoal "${COMP_WORDS[0]}" "${COMP_WORDS[COMP_CWORD]}" "${COMP_WORDS[COMP_CWORD-1]}"; printf '%s\n' "${COMPREPLY[@]}"
COMP_WORDS=(shoal codex --a); COMP_CWORD=2; COMP_LINE='shoal codex --a'; COMP_POINT=${#COMP_LINE}
_clap_complete_shoal "${COMP_WORDS[0]}" "${COMP_WORDS[COMP_CWORD]}" "${COMP_WORDS[COMP_CWORD-1]}"; printf '%s\n' "${COMPREPLY[@]}"
COMP_WORDS=(shoal doctor --re); COMP_CWORD=2; COMP_LINE='shoal doctor --re'; COMP_POINT=${#COMP_LINE}
_clap_complete_shoal "${COMP_WORDS[0]}" "${COMP_WORDS[COMP_CWORD]}" "${COMP_WORDS[COMP_CWORD-1]}"; printf '%s\n' "${COMPREPLY[@]}"
"#, "completion-test"])
        .arg(&script).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = String::from_utf8(output.stdout).unwrap();
    for expected in ["codex", "completions", "--cli", "--app", "--repair"] {
        assert!(
            output.lines().any(|line| line == expected),
            "missing {expected}: {output}"
        );
    }
    assert!(!home.path().join("state").exists());
}

#[test]
fn shell_init_registers_completions_for_bash_and_zsh() {
    let home = tempfile::tempdir().unwrap();
    let script = home.path().join("init.sh");
    fs::write(&script, generate(home.path(), &["shell", "init"])).unwrap();
    let bin = Path::new(env!("CARGO_BIN_EXE_shoal")).parent().unwrap();
    for (shell, check) in [
        (
            "bash",
            "complete -p shoal | command grep -q _clap_complete_shoal",
        ),
        (
            "zsh",
            "[[ ${_comps[shoal]} == _clap_dynamic_completer_shoal ]] && (( $+functions[_clap_dynamic_completer_shoal] ))",
        ),
    ] {
        let output = support::isolated(home.path(), shell)
            .args(["-f", "-c", &format!("source \"$1\"; {check}"), "init-test"])
            .arg(&script)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{shell}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let json: serde_json::Value =
        serde_json::from_slice(&generate(home.path(), &["--json", "completions", "zsh"])).unwrap();
    assert!(json["script"].as_str().unwrap().contains("#compdef shoal"));
}

#[test]
fn dynamic_completion_covers_nested_commands_flags_and_paths_without_daemon() {
    let home = tempfile::tempdir().unwrap();
    fs::create_dir_all(home.path().join(".config/shoal")).unwrap();
    fs::write(
        home.path().join(".config/shoal/config.toml"),
        "[commands]\nreview = ['tuicr']\n[ai.pi]\nskill_dir = '~/pi-skills'\n\
         [ai.droid]\ncommand = ['droid']\n",
    )
    .unwrap();
    install_programs(home.path(), &["claude", "codex", "happy", "tuicr", "droid"]);
    for (words, expected) in [
        (vec!["shoal", "repo", "r"], "rm"),
        (vec!["shoal", "rev"], "review"),
        (vec!["shoal", "don"], "done"),
        (vec!["shoal", "resu"], "resume"),
        (vec!["shoal", "resume", "--e"], "--execution"),
        (vec!["shoal", "resume", "--a"], "--all"),
        (vec!["shoal", "stop", "--a"], "--all"),
        (vec!["shoal", "wa"], "watch"),
        (vec!["shoal", "lin"], "link"),
        (vec!["shoal", "unl"], "unlink"),
        (vec!["shoal", "watch", "--w"], "--workspace"),
        (vec!["shoal", "done", "--k"], "--keep"),
        (vec!["shoal", "done", "--c"], "--cleanup"),
        (vec!["shoal", "run", "rev"], "review"),
        (vec!["shoal", "ins"], "install"),
        (vec!["shoal", "set"], "setup"),
        (vec!["shoal", "config", "i"], "install"),
        (vec!["shoal", "config", "s"], "show"),
        (vec!["shoal", "config", "install", "d"], "default"),
        (vec!["shoal", "repo", "c"], "config"),
        (vec!["shoal", "repo", "config", "--f"], "--file"),
        (vec!["shoal", "repo", "rm", "--y"], "--yes"),
        (vec!["shoal", "port", "a"], "acquire"),
        (vec!["shoal", "resource", "a"], "acquire"),
        (vec!["shoal", "sim", "a"], "acquire"),
        (vec!["shoal", "daemon", "re"], "restart"),
        (vec!["shoal", "codex", "--a"], "--app"),
        (vec!["shoal", "add", "--agent", "co"], "codex"),
        (vec!["shoal", "dro"], "droid"),
        (vec!["shoal", "add", "--agent", "dro"], "droid"),
        (vec!["shoal", "review", "--agent", "cl"], "claude"),
        (vec!["shoal", "add", "--agent", "cl"], "claude"),
        (vec!["shoal", "add", "--agent", "happy-cl"], "happy-claude"),
        (vec!["shoal", "happy", "co"], "codex"),
        (vec!["shoal", "skill", "install", "co"], "codex"),
        (vec!["shoal", "skill", "install", "pi"], "pi"),
    ] {
        for shell in ["bash", "zsh"] {
            let values = candidates(completion(home.path(), shell, &words));
            assert!(
                values.iter().any(|value| value == expected),
                "{shell} {words:?}: {values:?}"
            );
        }
    }
    assert!(!home.path().join("state").exists());
}

#[test]
fn agents_without_installed_executables_are_not_completed() {
    let home = tempfile::tempdir().unwrap();
    install_programs(home.path(), &["claude", "tuicr"]);
    fs::create_dir_all(home.path().join(".config/shoal")).unwrap();
    fs::write(
        home.path().join(".config/shoal/config.toml"),
        "[commands]\nreview = ['tuicr']\n[ai.droid]\ncommand = ['droid']\n",
    )
    .unwrap();
    // Only the fixture's bin, so agents installed on this machine stay out.
    let path = format!("{}:/usr/bin:/bin", home.path().join("bin").display());
    for (words, expected, missing) in [
        (
            vec!["shoal", "c"],
            &["claude", "completions"][..],
            &["codex"][..],
        ),
        (vec!["shoal", "cod"], &[], &["codex"]),
        (vec!["shoal", "ha"], &[], &["happy"]),
        (vec!["shoal", "t"], &[], &["t3"]),
        (vec!["shoal", "dr"], &[], &["droid"]),
        // Plain configured commands are not agents.
        (vec!["shoal", "add", "--agent", "r"], &[], &["review"]),
        (vec!["shoal", "add", "--agent", "d"], &[], &["droid"]),
        (vec!["shoal", "codex", "--a"], &[], &["--app"]),
        (
            vec!["shoal", "add", "--agent", "c"],
            &["claude"],
            &["codex"],
        ),
        (
            vec!["shoal", "add", "--agent", "happy-"],
            &[],
            &["happy-claude"],
        ),
    ] {
        for shell in ["bash", "zsh"] {
            let mut command = completion(home.path(), shell, &words);
            command.env("PATH", &path);
            let values = candidates(command);
            for value in expected {
                assert!(
                    values.contains(&value.to_string()),
                    "{shell} {words:?}: {values:?}"
                );
            }
            for value in missing {
                assert!(
                    !values.contains(&value.to_string()),
                    "{shell} {words:?}: {values:?}"
                );
            }
        }
    }
}

#[test]
fn zsh_completion_function_invokes_current_binary() {
    let home = tempfile::tempdir().unwrap();
    let script = home.path().join("completion.zsh");
    fs::write(&script, generate(home.path(), &["completions", "zsh"])).unwrap();
    let output = support::isolated(home.path(), "zsh")
        .args([
            "-f",
            "-c",
            r#"
autoload -Uz compinit; compinit -D
source "$1"
zstyle -s ':completion::complete:shoal::' sort sort_order
[[ $sort_order == false ]] || exit 1
_describe() { print -rl -- "${(@P)3}"; }
words=(shoal repo r); CURRENT=3
_clap_dynamic_completer_shoal
words=(shoal repo rm --y); CURRENT=4
_clap_dynamic_completer_shoal
"#,
            "completion-test",
        ])
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.lines().any(|line| line.starts_with("rm:")), "{text}");
    assert!(
        text.lines().any(|line| line.starts_with("--yes:")),
        "{text}"
    );
}

#[test]
fn doctor_detects_integration_in_the_calling_shell_without_a_daemon() {
    let home = tempfile::tempdir_in("/tmp").unwrap();
    let script = home.path().join("init.sh");
    fs::write(&script, generate(home.path(), &["shell", "init"])).unwrap();
    let bin = Path::new(env!("CARGO_BIN_EXE_shoal")).parent().unwrap();
    for shell in ["bash", "zsh"] {
        for loaded in [false, true] {
            let command = if loaded {
                r#"source "$1"; shoal --json doctor --all"#
            } else {
                "shoal --json doctor --all"
            };
            let output = support::isolated(home.path(), shell)
                .args(["-f", "-c", command, "doctor-test"])
                .arg(&script)
                .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(2), "{shell}: {output:?}");
            let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            let check = report["checks"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["name"] == "shell_integration")
                .unwrap();
            assert_eq!(check["status"], if loaded { "ok" } else { "warning" });
            if !loaded {
                assert!(
                    check["message"]
                        .as_str()
                        .unwrap()
                        .contains("source <(shoal shell init)")
                );
            }
        }
    }
    assert!(!home.path().join("state").exists());
}

#[test]
fn command_listing_keeps_global_commands_without_starting_a_daemon() {
    let home = tempfile::tempdir().unwrap();
    fs::create_dir_all(home.path().join(".config/shoal")).unwrap();
    fs::write(
        home.path().join(".config/shoal/config.toml"),
        "[commands]\nglobal-only = ['true']\n",
    )
    .unwrap();
    let output = generate(home.path(), &["--json", "run"]);
    let commands: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert!(
        commands
            .as_array()
            .unwrap()
            .iter()
            .any(|command| command["name"] == "global-only")
    );
    assert!(!home.path().join("state").exists());
}
