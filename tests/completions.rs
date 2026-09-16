use std::{fs, path::Path, process::Command};

fn generate(home: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new(env!("CARGO_BIN_EXE_shoal"))
        .args(args)
        .env("HOME", home)
        .env("SHOAL_STATE_DIR", home.join("state"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[test]
fn bash_completes_commands_flags_and_codex_modes_without_a_daemon() {
    let home = tempfile::tempdir().unwrap();
    let script = home.path().join("completions.bash");
    fs::write(&script, generate(home.path(), &["completions", "bash"])).unwrap();
    let output = Command::new("bash").args(["--noprofile", "--norc", "-c", r#"
source "$1"
COMP_TYPE=9
COMP_WORDS=(shoal co); COMP_CWORD=1; COMP_LINE='shoal co'; COMP_POINT=${#COMP_LINE}
_clap_complete_shoal "${COMP_WORDS[0]}" "${COMP_WORDS[COMP_CWORD]}" "${COMP_WORDS[COMP_CWORD-1]}"; printf '%s\n' "${COMPREPLY[@]}"
COMP_WORDS=(shoal codex c); COMP_CWORD=2; COMP_LINE='shoal codex c'; COMP_POINT=${#COMP_LINE}
_clap_complete_shoal "${COMP_WORDS[0]}" "${COMP_WORDS[COMP_CWORD]}" "${COMP_WORDS[COMP_CWORD-1]}"; printf '%s\n' "${COMPREPLY[@]}"
COMP_WORDS=(shoal codex a); COMP_CWORD=2; COMP_LINE='shoal codex a'; COMP_POINT=${#COMP_LINE}
_clap_complete_shoal "${COMP_WORDS[0]}" "${COMP_WORDS[COMP_CWORD]}" "${COMP_WORDS[COMP_CWORD-1]}"; printf '%s\n' "${COMPREPLY[@]}"
COMP_WORDS=(shoal reconcile --re); COMP_CWORD=2; COMP_LINE='shoal reconcile --re'; COMP_POINT=${#COMP_LINE}
_clap_complete_shoal "${COMP_WORDS[0]}" "${COMP_WORDS[COMP_CWORD]}" "${COMP_WORDS[COMP_CWORD-1]}"; printf '%s\n' "${COMPREPLY[@]}"
"#, "completion-test"])
        .arg(&script).env("HOME", home.path()).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = String::from_utf8(output.stdout).unwrap();
    for expected in ["codex", "completions", "cli", "app", "--repair"] {
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
        let output = Command::new(shell)
            .args(["-f", "-c", &format!("source \"$1\"; {check}"), "init-test"])
            .arg(&script)
            .env("HOME", home.path())
            .env("ZDOTDIR", home.path())
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
    for (words, expected) in [
        (vec!["shoal", "repo", "r"], "rm"),
        (vec!["shoal", "repo", "c"], "config"),
        (vec!["shoal", "repo", "config", "--f"], "--file"),
        (vec!["shoal", "repo", "rm", "--y"], "--yes"),
        (vec!["shoal", "port", "r"], "reserve"),
        (vec!["shoal", "resource", "a"], "acquire"),
        (vec!["shoal", "sim", "a"], "acquire"),
        (vec!["shoal", "daemon", "re"], "restart"),
        (vec!["shoal", "codex", "a"], "app"),
        (vec!["shoal", "skill", "install", "co"], "codex"),
    ] {
        for shell in ["bash", "zsh"] {
            let output = Command::new(env!("CARGO_BIN_EXE_shoal"))
                .arg("--")
                .args(&words)
                .env("SHOAL_COMPLETE", shell)
                .env("_CLAP_COMPLETE_INDEX", (words.len() - 1).to_string())
                .env("HOME", home.path())
                .env("SHOAL_STATE_DIR", home.path().join("state"))
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let text = String::from_utf8(output.stdout).unwrap();
            assert!(
                text.lines()
                    .any(|line| line.split(':').next() == Some(expected)),
                "{shell} {words:?}: {text}"
            );
        }
    }
    assert!(!home.path().join("state").exists());
}

#[test]
fn zsh_completion_function_invokes_current_binary() {
    let home = tempfile::tempdir().unwrap();
    let script = home.path().join("completion.zsh");
    fs::write(&script, generate(home.path(), &["completions", "zsh"])).unwrap();
    let output = Command::new("zsh")
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
        .env("HOME", home.path())
        .env("ZDOTDIR", home.path())
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
