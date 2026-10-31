use super::*;

#[test]
fn hosts_pin_a_known_type_case_insensitively() {
    let forges: Forges = toml::from_str("[\"Git.Example.com\"]\ntype = \"gitlab\"\n").unwrap();
    forges.validate().unwrap();
    assert_eq!(forges.pinned("git.example.com"), Some(ForgeKind::GitLab));
    assert_eq!(forges.pinned("other.example.com"), None);
    assert!(toml::from_str::<Forges>("[\"git.example.com\"]\ntype = \"svn\"\n").is_err());
    assert!(toml::from_str::<Forges>("[\"git.example.com\"]\nkind = \"github\"\n").is_err());
    let url: Forges = toml::from_str("[\"https://git.example.com\"]\ntype = \"github\"\n").unwrap();
    assert!(url.validate().is_err());
    let twice: Forges = toml::from_str(
        "[\"Git.example.com\"]\ntype = \"github\"\n[\"git.example.com\"]\ntype = \"gitlab\"\n",
    )
    .unwrap();
    assert!(twice.validate().is_err());
}

#[test]
fn tokens_take_the_shared_value_forms_and_emails_only_on_bitbucket() {
    let forges: Forges = toml::from_str(
        r#"
["git.example.com"]
token = { cmd = ["security", "find-generic-password", "-w"] }
agent_token = { env = "AGENT_TOKEN" }

["bitbucket.org"]
token = "written"
email = "me@example.com"
"#,
    )
    .unwrap();
    forges.validate().unwrap();
    assert_eq!(forges.pinned("git.example.com"), None);
    assert!(forges.pins().is_empty());
    assert_eq!(
        forges.identity(Whose::Agent),
        "git.example.com Env { env: \"AGENT_TOKEN\" } None"
    );
    // A written token never shows, though a changed one names another account.
    assert!(!format!("{forges:?}").contains("written"));
    let user = forges.identity(Whose::User);
    assert!(!user.contains("\"written\""), "{user}");
    let changed: Forges =
        toml::from_str("[\"bitbucket.org\"]\ntoken = \"other\"\nemail = \"me@example.com\"\n")
            .unwrap();
    let identity = changed.identity(Whose::User);
    assert!(identity.starts_with("bitbucket.org written "), "{identity}");
    assert!(!user.contains(&identity), "{user}");

    for (text, error) in [
        (
            "[\"git.example.com\"]\ntoken = \"t\"\nemail = \"me@example.com\"\n",
            "is for Bitbucket API tokens",
        ),
        (
            "[\"bitbucket.org\"]\nagent_email = \"me@example.com\"\n",
            "agent_email needs machine.forges.\"bitbucket.org\".agent_token",
        ),
        (
            "[\"bitbucket.org\"]\ntoken = \"t\"\nemail = \"me\"\n",
            "must be an email address",
        ),
        (
            "[\"git.example.com\"]\ntoken = { cmd = [] }\n",
            "token.cmd must name a program",
        ),
    ] {
        let forges: Forges = toml::from_str(text).unwrap();
        let message = format!("{:#}", forges.validate().unwrap_err());
        assert!(message.contains(error), "{message}");
    }
}

#[tokio::test]
async fn tokens_fetch_their_value_and_name_the_setting_when_they_fail() {
    let configured = |value| Configured {
        setting: "machine.forges.\"git.example.com\".agent_token".into(),
        value,
    };
    let written = configured(Value::Written("abc".into()));
    assert_eq!(written.fetch().await.unwrap(), "abc");
    let failing = configured(Value::Cmd {
        cmd: vec!["false".into()],
    });
    let error = failing.fetch().await.unwrap_err();
    assert!(
        format!("{error:#}").contains("read machine.forges.\"git.example.com\".agent_token"),
        "{error:#}"
    );
}

#[tokio::test]
async fn token_commands_report_their_diagnostics_and_are_bounded() {
    let command = |script: &str| vec!["sh".to_owned(), "-c".to_owned(), script.to_owned()];
    let error = token_output(&command("echo locked >&2; exit 3"))
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("locked"), "{error:#}");
    let flood = command("head -c 70000 /dev/zero | tr '\\0' a");
    assert!(token_output(&flood).await.is_err());
    assert_eq!(token_output(&command("echo abc")).await.unwrap(), "abc\n");
}
