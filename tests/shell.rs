mod support;

use std::{fs, os::unix::fs::PermissionsExt};

#[test]
fn shell_recovers_after_cleanup_without_losing_command_status() {
    let root = tempfile::tempdir().unwrap();
    let integration = root.path().join("init.sh");
    let output = support::cli(root.path())
        .args(["shell", "init"])
        .output()
        .unwrap();
    assert!(output.status.success());
    fs::write(&integration, output.stdout).unwrap();
    fs::create_dir(root.path().join("bin")).unwrap();
    let stub = root.path().join("bin/shoal");
    fs::write(
        &stub,
        r#"#!/bin/sh
case "$1" in
  cleanup) rm -rf -- "$REMOVED"; exit 7 ;;
  stale)
    printf '%s\n' "$REMOVED" > "$SHOAL_SHELL_DIRECTIVE"
    rm -rf -- "$REMOVED"
    exit 7 ;;
  *) exec "$TEST_SHOAL" "$@" ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();

    for shell in ["bash", "zsh"] {
        let output = support::isolated(root.path(), shell)
            .args([
                "-f",
                "-c",
                r#"
set -eu
old_prompt() { :; }
if [ -n "${ZSH_VERSION-}" ]; then
  precmd_functions=(old_prompt)
else
  PROMPT_COMMAND='old_prompt'
fi
. "$INTEGRATION"
. "$INTEGRATION"
if [ -n "${ZSH_VERSION-}" ]; then
  test "${precmd_functions[*]}" = '_shoal_recover_directory old_prompt'
else
  test "$PROMPT_COMMAND" = '_shoal_recover_directory; old_prompt'
  PROMPT_COMMAND=(old_prompt 'old_prompt second')
  . "$INTEGRATION"
  test "${#PROMPT_COMMAND[@]}" -eq 3
  test "${PROMPT_COMMAND[2]}" = 'old_prompt second'
fi
# Literal paths must never become shell code, including after deletion.
parent="$HOME/repo \$(touch injected) 'quoted'"
mkdir -p "$parent"
export REMOVED="$parent/workspace"
mkdir -p "$REMOVED/nested"
cd "$REMOVED/nested"
if shoal cleanup; then exit 1; else test "$?" -eq 7; fi
test "$PWD" = "$parent"
test ! -e "$HOME/injected"
# add --agent may record a destination that cleanup removes before return.
mkdir -p "$REMOVED"
cd "$HOME"
if shoal stale; then exit 1; else test "$?" -eq 7; fi
test "$PWD" = "$parent"
# Cleanup can finish after the wrapper returns; the next prompt recovers too.
mkdir -p "$REMOVED/nested"
cd "$REMOVED/nested"
rm -rf -- "$REMOVED"
if (exit 9); then exit 1; else
  if _shoal_recover_directory; then exit 1; else test "$?" -eq 9; fi
fi
test "$PWD" = "$parent"
# An existing current directory is left alone.
mkdir -p "$REMOVED"
cd "$REMOVED"
_shoal_recover_directory
test "$PWD" = "$REMOVED"
# Scoped shells cannot escape their removed workspace.
export SHOAL_SCOPE_TOKEN=test-scope
rm -rf -- "$REMOVED"
_shoal_recover_directory
test "$PWD" = "$REMOVED"
if command shoal shell recover -- "$REMOVED"; then exit 1; fi
unset SHOAL_SCOPE_TOKEN
_shoal_recover_directory
test "$PWD" = "$parent"
printf 'recovery-ok\n'
"#,
            ])
            .env("INTEGRATION", &integration)
            .env("TEST_SHOAL", env!("CARGO_BIN_EXE_shoal"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{shell}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).ends_with("recovery-ok\n"));
    }
    assert!(!root.path().join("state").exists());
}

#[test]
fn interactive_prompt_recovers_cleanup_that_finishes_after_command_return() {
    use std::{io::Write, process::Stdio};

    let root = tempfile::tempdir().unwrap();
    let integration = root.path().join("init.sh");
    let output = support::cli(root.path())
        .args(["shell", "init"])
        .output()
        .unwrap();
    assert!(output.status.success());
    fs::write(&integration, output.stdout).unwrap();
    let bin = std::path::Path::new(env!("CARGO_BIN_EXE_shoal"))
        .parent()
        .unwrap();
    for shell in ["bash", "zsh"] {
        let mut child = support::isolated(root.path(), shell)
            .args(["-f", "-i"])
            .env("INTEGRATION", &integration)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(
                br#"
old_prompt() { printf 'hook-status:%s\n' "$?"; }
if [ -n "${ZSH_VERSION-}" ]; then precmd_functions=(old_prompt); else PROMPT_COMMAND=old_prompt; fi
. "$INTEGRATION"
mkdir -p "$HOME/workspace/nested"
cd "$HOME/workspace/nested"
rm -rf -- "$HOME/workspace"; (exit 7)
if [ "$PWD" = "$HOME" ]; then printf 'prompt-recovered\n'; fi
exit
"#,
            )
            .unwrap();
        let output = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success()
                && stdout.contains("hook-status:7\n")
                && stdout.contains("prompt-recovered\n"),
            "{shell}: {stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert!(!root.path().join("state").exists());
}
