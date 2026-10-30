use super::*;

fn profile(text: &str) -> Result<Vec<(String, String)>> {
    toml::from_str::<Profile>(text)?.settings()
}

#[test]
fn settings_flatten_dotted_keys_and_tables_in_git_spelling() {
    let settings = profile(
        "user.name = 'Name'\ncommit.gpgsign = true\n[gpg]\nformat = 'ssh'\n\
             [url.'https://x/']\ninsteadOf = 'git@x:'\n[http]\npostBuffer = 5\n",
    )
    .unwrap();
    assert_eq!(
        settings,
        [
            ("commit.gpgsign", "true"),
            ("gpg.format", "ssh"),
            ("http.postBuffer", "5"),
            ("url.https://x/.insteadOf", "git@x:"),
            ("user.name", "Name"),
        ]
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
    );
    for text in [
        "",
        "user = 'flat'",
        "user.email = 1.5",
        "user.email = ['a']",
        "'bad section'.email = 'x'",
        "user.'bad name' = 'x'",
        "user.-name = 'x'",
        "core.worktree = '/elsewhere'",
        "core.bare = true",
        "extensions.worktreeConfig = false",
        "user.email = \"a\\u0000b\"",
    ] {
        assert!(profile(text).is_err(), "{text}");
    }
}

#[tokio::test]
async fn incompatible_shared_layout_is_not_changed() {
    let root = tempfile::tempdir().unwrap();
    git::run(root.path(), &["init", "-b", "main"])
        .await
        .unwrap();
    let profile: Profile = toml::from_str("user.email = 'work@example.invalid'").unwrap();
    for (key, value) in [("core.bare", "true"), ("core.worktree", "/elsewhere")] {
        git::run(root.path(), &["config", "--local", key, value])
            .await
            .unwrap();
        assert!(
            apply(root.path(), &profile)
                .await
                .unwrap_err()
                .to_string()
                .contains("shared Git config")
        );
        assert_eq!(
            git::run(
                root.path(),
                &[
                    "config",
                    "--local",
                    "--default",
                    "false",
                    "--get",
                    "extensions.worktreeConfig"
                ]
            )
            .await
            .unwrap()
            .trim(),
            "false"
        );
        assert!(!root.path().join(".git/config.worktree").exists());
        git::run(root.path(), &["config", "--local", "--unset", key])
            .await
            .unwrap();
    }
}

#[test]
fn profiles_are_looked_up_by_portable_name() {
    let git: Git =
        toml::from_str("[profiles.work]\nuser.email = 'w@x'\n[profiles.oss]\nuser.email = 'o@x'\n")
            .unwrap();
    git.validate().unwrap();
    assert_eq!(git.profile("work").unwrap().settings().unwrap()[0].1, "w@x");
    let error = git.profile("home").unwrap_err().to_string();
    assert!(error.contains("oss, work"), "{error}");
    assert!(
        Git::default()
            .profile("home")
            .unwrap_err()
            .to_string()
            .contains("[machine.git.profiles.home]")
    );
    for text in [
        "[profiles.'bad name']\nuser.email = 'x'\n",
        "[profiles.empty]\n",
        "[profile.work]\nuser.email = 'x'\n",
    ] {
        let parsed = toml::from_str::<Git>(text)
            .map_err(anyhow::Error::from)
            .and_then(|git| git.validate());
        assert!(parsed.is_err(), "{text}");
    }
}
