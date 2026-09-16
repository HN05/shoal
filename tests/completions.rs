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
COMP_WORDS=(shoal co); COMP_CWORD=1; COMP_LINE='shoal co'; COMP_POINT=${#COMP_LINE}
_shoal "${COMP_WORDS[0]}" "${COMP_WORDS[COMP_CWORD]}" "${COMP_WORDS[COMP_CWORD-1]}"; printf '%s\n' "${COMPREPLY[@]}"
COMP_WORDS=(shoal codex c); COMP_CWORD=2; COMP_LINE='shoal codex c'; COMP_POINT=${#COMP_LINE}
_shoal "${COMP_WORDS[0]}" "${COMP_WORDS[COMP_CWORD]}" "${COMP_WORDS[COMP_CWORD-1]}"; printf '%s\n' "${COMPREPLY[@]}"
COMP_WORDS=(shoal codex a); COMP_CWORD=2; COMP_LINE='shoal codex a'; COMP_POINT=${#COMP_LINE}
_shoal "${COMP_WORDS[0]}" "${COMP_WORDS[COMP_CWORD]}" "${COMP_WORDS[COMP_CWORD-1]}"; printf '%s\n' "${COMPREPLY[@]}"
COMP_WORDS=(shoal reconcile --re); COMP_CWORD=2; COMP_LINE='shoal reconcile --re'; COMP_POINT=${#COMP_LINE}
_shoal "${COMP_WORDS[0]}" "${COMP_WORDS[COMP_CWORD]}" "${COMP_WORDS[COMP_CWORD-1]}"; printf '%s\n' "${COMPREPLY[@]}"
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
        ("bash", "complete -p shoal | command grep -q _shoal"),
        (
            "zsh",
            "[[ ${_comps[shoal]} == _shoal ]] && (( $+functions[_shoal] ))",
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
