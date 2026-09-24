use super::permits::RESOURCE_CONFIG;
use crate::support::Fixture;
use std::{fs, path::Path};

#[test]
fn live_completion_uses_targets_state_override_workspace_context_and_scope() {
    let fixture = Fixture::with_config(Some(RESOURCE_CONFIG));
    fixture.ok(&["repo", "rename", fixture.repo.to_str().unwrap(), "project"]);
    let first = fixture.add("first");
    fixture.add("second");
    fixture.ok(&["port", "acquire", "web", "first"]);
    fixture.ok(&["resource", "acquire", "devices", "first", "--name", "tests"]);
    let complete = |args: &[&str], cwd: &Path| {
        let state = fixture.root.path().join("state");
        let mut words = vec!["shoal", "--state-dir", state.to_str().unwrap()];
        words.extend_from_slice(args);
        let output = fixture
            .command()
            .arg("--")
            .args(&words)
            .env("SHOAL_COMPLETE", "bash")
            .env("_CLAP_COMPLETE_INDEX", (words.len() - 1).to_string())
            .env("SHOAL_STATE_DIR", fixture.root.path().join("wrong-state"))
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    for args in [
        vec!["repo", "rm", "pr"],
        vec!["repo", "config", "pr"],
        vec!["repo", "rename", "pr"],
        vec!["add", "pr"],
        vec!["issue", "103", "--repo", "pr"],
    ] {
        assert!(
            complete(&args, fixture.root.path()).contains(&"project".into()),
            "{args:?}"
        );
    }
    for command in ["rm", "cd", "exec", "diff", "status", "inspect"] {
        assert!(
            complete(&[command, "fi"], fixture.root.path()).contains(&"first".into()),
            "{command}"
        );
    }
    let choices = complete(&["repo", "rm", ""], fixture.root.path());
    let target = choices.iter().position(|v| v == "project").unwrap();
    assert!(
        choices
            .iter()
            .enumerate()
            .filter(|(_, v)| v.starts_with('-'))
            .all(|(i, _)| i > target)
    );
    let cwd = Path::new(first["path"].as_str().unwrap());
    fs::write(cwd.join(".shoal.toml"), "[commands]\nreview = ['tuicr']\n").unwrap();
    // Complete the command name with an unknown workspace already on the line.
    let names = fixture
        .command()
        .env("SHOAL_STATE_DIR", fixture.root.path().join("state"))
        .args(["--", "shoal", "run", "rev", "unknown"])
        .env("SHOAL_COMPLETE", "bash")
        .env("_CLAP_COMPLETE_INDEX", "2")
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(names.status.success(), "{names:?}");
    assert!(
        String::from_utf8_lossy(&names.stdout)
            .lines()
            .any(|line| line == "review")
    );
    assert!(complete(&["rev"], cwd).contains(&"review".into()));
    assert!(complete(&["review", "fi"], cwd).contains(&"first".into()));
    assert!(complete(&["review", "fi"], fixture.root.path()).contains(&"first".into()));
    assert!(complete(&["port", "release", "w"], cwd).contains(&"web".into()));
    assert!(complete(&["resource", "acquire", "d"], cwd).contains(&"devices".into()));
    assert!(
        complete(
            &["resource", "acquire", "devices", "first", "--resource", "b"],
            fixture.root.path()
        )
        .contains(&"beta".into())
    );
    assert!(
        complete(
            &["resource", "release", "devices", "first", "--name", "t"],
            fixture.root.path()
        )
        .contains(&"tests".into())
    );
    assert!(
        complete(
            &[
                "resource",
                "acquire",
                "devices",
                first["id"].as_str().unwrap(),
                "--resource",
                "b"
            ],
            fixture.root.path(),
        )
        .contains(&"beta".into())
    );
    assert!(
        !complete(
            &[
                "resource",
                "acquire",
                "devices",
                "unknown",
                "--resource",
                "b"
            ],
            cwd,
        )
        .contains(&"beta".into())
    );
    fixture.ok(&["rm", "second"]);
    assert!(!complete(&["rm", ""], cwd).contains(&"second".into()));
    fixture.add("second");
    let scoped = fixture.run(&[
        "exec",
        "first",
        "--",
        "env",
        "SHOAL_COMPLETE=bash",
        "_CLAP_COMPLETE_INDEX=2",
        env!("CARGO_BIN_EXE_shoal"),
        "--",
        "shoal",
        "rm",
        "",
    ]);
    assert!(
        scoped.status.success(),
        "{}",
        String::from_utf8_lossy(&scoped.stderr)
    );
    let text = String::from_utf8(scoped.stdout).unwrap();
    assert!(text.lines().any(|line| line == "first"), "{text}");
    assert!(!text.lines().any(|line| line == "second"), "{text}");
    assert!(!fixture.root.path().join("wrong-state").exists());
}
