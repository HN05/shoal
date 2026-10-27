use serde_json::json;

use super::*;

const URL: &str = "http://127.0.0.1:4000";

fn args(args: &[&str]) -> Vec<OsString> {
    args.iter().map(Into::into).collect()
}

/// The one `--settings` `merged` leaves, parsed.
fn settings(merged: &[OsString]) -> Value {
    let at = merged.iter().position(|arg| arg == "--settings").unwrap();
    assert_eq!(
        merged.iter().filter(|arg| *arg == "--settings").count(),
        1,
        "{merged:?}"
    );
    serde_json::from_str(merged[at + 1].to_str().unwrap()).unwrap()
}

#[test]
fn a_users_settings_join_shoals_and_the_proxy_url_wins() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("mine.json"),
        r#"{"env":{"ANTHROPIC_BASE_URL":"http://elsewhere","FOO":"bar"},"model":"opus"}"#,
    )
    .unwrap();
    let shoal = json!({
        "hooks": {"Stop": [{"hooks": [{"command": "shoal"}]}]},
        "env": {"ANTHROPIC_BASE_URL": URL},
    })
    .to_string();
    let user = json!({
        "hooks": {"Stop": [{"hooks": [{"command": "mine"}]}]},
        "permissions": {"allow": ["Bash(ls)"]},
    })
    .to_string();
    // A forwarded `--settings` after Shoal's, in either form, and a file.
    for (forwarded, file) in [(vec!["--settings", user.as_str()], false), (vec![], true)] {
        let mut given = args(&["claude", "--settings", &shoal, "--continue"]);
        given.extend(forwarded.iter().map(Into::into));
        if file {
            given.push("--settings=mine.json".into());
        }
        given.extend(args(&["--", "--settings", "prompt"]));
        merge(&mut given, URL, directory.path()).unwrap();
        let merged = &given[..given.iter().position(|arg| arg == "--").unwrap()];
        let settings = settings(merged);
        assert_eq!(settings["env"]["ANTHROPIC_BASE_URL"], URL, "{settings}");
        assert_eq!(merged[..2], args(&["claude", "--settings"])[..]);
        assert_eq!(merged[3], "--continue");
        if file {
            assert_eq!(settings["env"]["FOO"], "bar");
            assert_eq!(settings["model"], "opus");
        } else {
            // Both hooks of the event run, Shoal's first.
            let stop = &settings["hooks"]["Stop"];
            assert_eq!(stop[0]["hooks"][0]["command"], "shoal");
            assert_eq!(stop[1]["hooks"][0]["command"], "mine");
            assert_eq!(settings["permissions"]["allow"][0], "Bash(ls)");
        }
        // The prompt after `--` is left alone.
        let end = given.iter().position(|arg| arg == "--").unwrap();
        assert_eq!(given[end..], args(&["--", "--settings", "prompt"])[..]);
    }
}

#[test]
fn settings_that_cannot_be_read_fail_the_launch() {
    let directory = tempfile::tempdir().unwrap();
    for (value, message) in [
        ("missing.json", "read Claude settings"),
        ("{not json", "parse Claude settings"),
        (r#"{"env":false}"#, "env is not a JSON object"),
        ("{}", ""),
    ] {
        let mut given = args(&["claude", "--settings", "{}", "--settings", value]);
        let result = merge(&mut given, URL, directory.path());
        match message {
            "" => assert_eq!(settings(&given)["env"]["ANTHROPIC_BASE_URL"], URL),
            message => {
                let error = format!("{:#}", result.unwrap_err());
                assert!(error.contains(message), "{value}: {error}");
            }
        }
    }
}
