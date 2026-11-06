mod support;

use std::{fs, path::Path, process::Command};

/// Agents complete only when their executables are installed.
fn install_programs(home: &Path, programs: &[&str]) {
    let bin = home.join("bin");
    for program in programs {
        let path = bin.join(program);
        support::install_test_script(&path, "#!/bin/sh\n");
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
    fs::write(
        &script,
        generate(home.path(), &["shell", "completions", "bash"]),
    )
    .unwrap();
    let output = support::isolated(home.path(), "bash").args(["--noprofile", "--norc", "-c", r#"
source "$1"
COMP_TYPE=9
COMP_WORDS=(shoal co); COMP_CWORD=1; COMP_LINE='shoal co'; COMP_POINT=${#COMP_LINE}
_clap_complete_shoal "${COMP_WORDS[0]}" "${COMP_WORDS[COMP_CWORD]}" "${COMP_WORDS[COMP_CWORD-1]}"; printf '%s\n' "${COMPREPLY[@]}"
COMP_WORDS=(shoal open --a); COMP_CWORD=2; COMP_LINE='shoal open --a'; COMP_POINT=${#COMP_LINE}
_clap_complete_shoal "${COMP_WORDS[0]}" "${COMP_WORDS[COMP_CWORD]}" "${COMP_WORDS[COMP_CWORD-1]}"; printf '%s\n' "${COMPREPLY[@]}"
COMP_WORDS=(shoal open --app t); COMP_CWORD=3; COMP_LINE='shoal open --app t'; COMP_POINT=${#COMP_LINE}
_clap_complete_shoal "${COMP_WORDS[0]}" "${COMP_WORDS[COMP_CWORD]}" "${COMP_WORDS[COMP_CWORD-1]}"; printf '%s\n' "${COMPREPLY[@]}"
COMP_WORDS=(shoal resume --di); COMP_CWORD=2; COMP_LINE='shoal resume --di'; COMP_POINT=${#COMP_LINE}
_clap_complete_shoal "${COMP_WORDS[0]}" "${COMP_WORDS[COMP_CWORD]}" "${COMP_WORDS[COMP_CWORD-1]}"; printf '%s\n' "${COMPREPLY[@]}"
"#, "completion-test"])
        .arg(&script).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = String::from_utf8(output.stdout).unwrap();
    for expected in ["codex", "--app", "t3", "--discard"] {
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
    let bin = Path::new(support::shoal_binary()).parent().unwrap();
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
}

#[test]
fn dynamic_completion_covers_nested_commands_flags_and_paths_without_daemon() {
    let home = tempfile::tempdir().unwrap();
    fs::create_dir_all(home.path().join(".config/shoal")).unwrap();
    fs::write(
        home.path().join(".config/shoal/config.toml"),
        "[commands]\nreview = ['tuicr']\n[agents.happy]\nenabled = true\n[agents.droid]\ncommand = ['droid']\n[machine.agents.pi]\nskill_dir = '~/pi-skills'\n",
    )
    .unwrap();
    // A mode only its file beside the config defines.
    fs::create_dir_all(home.path().join(".config/shoal/modes")).unwrap();
    fs::write(
        home.path().join(".config/shoal/modes/triage.md"),
        "Label it.",
    )
    .unwrap();
    install_programs(home.path(), &["claude", "codex", "happy", "tuicr", "droid"]);
    for (words, expected) in [
        (vec!["shoal", "repo", "r"], "rm"),
        (vec!["shoal", "run", "rev"], "review"),
        (vec!["shoal", "messages", "send", "hello", "--a"], "--all"),
        (vec!["shoal", "open", "--a"], "--app"),
        (vec!["shoal", "start", "--agent", "co"], "codex"),
        (vec!["shoal", "dro"], "droid"),
        (
            vec!["shoal", "start", "--agent", "happy-cl"],
            "happy-claude",
        ),
        (vec!["shoal", "skill", "install", "pi"], "pi"),
        (vec!["shoal", "start", "--mode", "tri"], "triage"),
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
    // Happy is installed but not enabled, so neither it nor its agents complete.
    install_programs(home.path(), &["claude", "tuicr", "happy"]);
    fs::create_dir_all(home.path().join(".config/shoal")).unwrap();
    fs::write(
        home.path().join(".config/shoal/config.toml"),
        "[commands]\nreview = ['tuicr']\n[agents.droid]\ncommand = ['droid']\n[agents.claude-direct]\nbase = 'claude'\n[agents.droid-direct]\nbase = 'droid'\n",
    )
    .unwrap();
    // Only the fixture's bin, so agents installed on this machine stay out.
    let path = format!("{}:/usr/bin:/bin", home.path().join("bin").display());
    for (words, expected, missing) in [
        (vec!["shoal", "c"], &["claude"][..], &["codex"][..]),
        (vec!["shoal", "ha"], &[], &["happy"]),
        (vec!["shoal", "dr"], &[], &["droid"]),
        // Plain configured commands are not agents.
        (vec!["shoal", "start", "--agent", "r"], &[], &["review"]),
        (
            vec!["shoal", "start", "--agent", "c"],
            &["claude", "claude-direct"],
            &["codex"],
        ),
        (
            vec!["shoal", "start", "-a", "c"],
            &["claude", "claude-direct"],
            &["codex"],
        ),
        // Launch profiles run their base's launcher.
        (
            vec!["shoal", "start", "--agent", "droid-"],
            &[],
            &["droid-direct"],
        ),
        (
            vec!["shoal", "start", "--agent", "happy-"],
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
fn agents_complete_only_when_enabled_and_installed() {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join(".config/shoal/config.toml");
    fs::create_dir_all(config.parent().unwrap()).unwrap();
    let path = format!("{}:/usr/bin:/bin", home.path().join("bin").display());
    let complete = |words: &[&str]| {
        let mut command = completion(home.path(), "bash", words);
        command.env("PATH", &path);
        candidates(command)
    };
    let agents = |prefix: &str| complete(&["shoal", "start", "--agent", prefix]);
    fs::write(&config, "[agents.happy]\nenabled = true\n").unwrap();
    // Happy alone runs no agent, so none of its agents complete.
    install_programs(home.path(), &["happy"]);
    for words in [
        &["shoal", "start", "--agent", "happy-"][..],
        &["shoal", "happy-"],
        &["shoal", "run", "happy-"],
    ] {
        assert_eq!(complete(words), Vec::<String>::new(), "{words:?}");
    }
    install_programs(home.path(), &["claude"]);
    assert_eq!(agents("happy-"), ["happy-claude"]);
    for words in [&["shoal", "happy-"][..], &["shoal", "run", "happy-"]] {
        assert!(
            complete(words).contains(&"happy-claude".to_owned()),
            "{words:?}"
        );
    }
    assert_eq!(agents("cl"), ["claude"]);
    // Happy's agents are off until enabled, and any agent can be turned off.
    fs::write(&config, "[agents.claude]\nenabled = false\n").unwrap();
    assert_eq!(agents("happy-"), Vec::<String>::new());
    assert_eq!(agents("cl"), Vec::<String>::new());
    for words in [&["shoal", "cl"][..], &["shoal", "run", "cl"]] {
        assert!(!complete(words).contains(&"claude".to_owned()), "{words:?}");
    }
}

#[test]
fn zsh_completion_function_invokes_current_binary() {
    let home = tempfile::tempdir().unwrap();
    let script = home.path().join("completion.zsh");
    fs::write(
        &script,
        generate(home.path(), &["shell", "completions", "zsh"]),
    )
    .unwrap();
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
