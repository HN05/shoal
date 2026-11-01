use std::ffi::OsStr;

use super::*;

#[test]
fn rewrites_send_pushes_through_the_helper() {
    let urls = git::RemoteUrls {
        fetch: vec!["git@forge:o/r.git".into(), "https://forge/o/fork".into()],
        push: vec!["ssh://forge/o/r".into()],
    };
    assert_eq!(
        rewrites(&urls).unwrap(),
        [
            (
                "url.shoal::git@forge:o/r.git.pushInsteadOf".to_owned(),
                "git@forge:o/r.git".to_owned()
            ),
            (
                "url.shoal::https://forge/o/fork.pushInsteadOf".to_owned(),
                "https://forge/o/fork".to_owned()
            ),
            (
                "url.shoal::ssh://forge/o/r.insteadOf".to_owned(),
                "ssh://forge/o/r".to_owned()
            ),
        ]
    );
}

#[test]
fn a_push_url_that_is_also_fetched_from_cannot_be_held() {
    let urls = git::RemoteUrls {
        fetch: vec!["git@forge:o/r.git".into()],
        push: vec!["git@forge:o/r.git".into()],
    };
    let error = rewrites(&urls).unwrap_err();
    assert!(
        format!("{error:#}").contains("also fetched from"),
        "{error:#}"
    );
}

#[test]
fn rewrites_are_recognized_whatever_the_case_of_their_names() {
    assert!(is_rewrite(OsStr::new(
        "url.shoal::git@forge:o/r.git.pushInsteadOf"
    )));
    assert!(is_rewrite(OsStr::new(
        "URL.shoal::https://forge/o/r.INSTEADOF"
    )));
    assert!(!is_rewrite(OsStr::new("url.https://mirror/.insteadOf")));
    assert!(!is_rewrite(OsStr::new("user.name")));
}

#[test]
fn strip_drops_the_rewrites_and_renumbers_the_other_settings() {
    let mut command = tokio::process::Command::new("git");
    for (index, (key, value)) in [
        ("user.name", "Agent"),
        (
            "url.shoal::git@forge:o/r.git.pushInsteadOf",
            "git@forge:o/r.git",
        ),
        ("commit.gpgsign", "true"),
    ]
    .into_iter()
    .enumerate()
    {
        command
            .env(format!("GIT_CONFIG_KEY_{index}"), key)
            .env(format!("GIT_CONFIG_VALUE_{index}"), value);
    }
    command.env("GIT_CONFIG_COUNT", "3");

    strip(&mut command);

    let get = |name: &str| {
        command
            .as_std()
            .get_envs()
            .find(|(set, _)| *set == name)
            .and_then(|(_, value)| value.map(|value| value.to_string_lossy().into_owned()))
    };
    assert_eq!(get("GIT_CONFIG_COUNT").as_deref(), Some("2"));
    assert_eq!(get("GIT_CONFIG_KEY_0").as_deref(), Some("user.name"));
    assert_eq!(get("GIT_CONFIG_KEY_1").as_deref(), Some("commit.gpgsign"));
    assert_eq!(get("GIT_CONFIG_VALUE_1").as_deref(), Some("true"));
    assert_eq!(get("GIT_CONFIG_KEY_2"), None);
}
