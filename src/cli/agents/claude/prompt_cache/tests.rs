use std::collections::BTreeMap;

use super::*;

/// The lifetime with `variables` set and `settings` as the user's Claude
/// settings, if any.
fn lifetime_with(variables: &[(&str, &str)], settings: Option<&str>) -> Duration {
    let home = tempfile::tempdir().unwrap();
    if let Some(settings) = settings {
        std::fs::create_dir(home.path().join(".claude")).unwrap();
        std::fs::write(home.path().join(".claude/settings.json"), settings).unwrap();
    }
    let variables: BTreeMap<_, _> = variables.iter().copied().collect();
    let variable = |name: &str| variables.get(name).map(OsString::from);
    lifetime(&variable, home.path())
}

#[test]
fn a_claude_ai_login_keeps_an_hour_and_a_credential_five_minutes() {
    assert_eq!(lifetime_with(&[], None), ONE_HOUR);
    // A base URL alone keeps the login, as through Shoal's proxy.
    assert_eq!(
        lifetime_with(&[("ANTHROPIC_BASE_URL", "http://127.0.0.1:9")], None),
        ONE_HOUR
    );
    for credential in [
        ("ANTHROPIC_AUTH_TOKEN", "sk-test"),
        ("ANTHROPIC_API_KEY", "sk-test"),
        ("CLAUDE_CODE_USE_BEDROCK", "1"),
    ] {
        assert_eq!(
            lifetime_with(&[credential], None),
            FIVE_MINUTES,
            "{credential:?}"
        );
    }
    assert_eq!(
        lifetime_with(&[], Some(r#"{"apiKeyHelper": "key"}"#)),
        FIVE_MINUTES
    );
    assert_eq!(
        lifetime_with(&[("CLAUDE_CODE_USE_BEDROCK", "0")], None),
        ONE_HOUR
    );
}

#[test]
fn a_chosen_lifetime_wins_as_claude_code_ranks_it() {
    let token = ("ANTHROPIC_AUTH_TOKEN", "sk-test");
    let enable = (super::super::PROMPT_CACHE_1H, "1");
    assert_eq!(lifetime_with(&[token, enable], None), ONE_HOUR);
    assert_eq!(
        lifetime_with(&[token], Some(r#"{"promptCacheTtl": "1h"}"#)),
        ONE_HOUR
    );
    assert_eq!(
        lifetime_with(&[enable], Some(r#"{"promptCacheTtl": "5m"}"#)),
        FIVE_MINUTES
    );
    assert_eq!(
        lifetime_with(
            &[("CLAUDE_CODE_PROMPT_CACHE_TTL", "1h")],
            Some(r#"{"promptCacheTtl": "5m"}"#)
        ),
        ONE_HOUR
    );
    assert_eq!(
        lifetime_with(
            &[
                ("FORCE_PROMPT_CACHING_5M", "1"),
                ("CLAUDE_CODE_PROMPT_CACHE_TTL", "1h")
            ],
            None
        ),
        FIVE_MINUTES
    );
    // Unreadable settings and unknown values decide nothing.
    assert_eq!(lifetime_with(&[token], Some("{")), FIVE_MINUTES);
    assert_eq!(
        lifetime_with(&[token, ("CLAUDE_CODE_PROMPT_CACHE_TTL", "2h")], None),
        FIVE_MINUTES
    );
}

#[test]
fn settings_follow_the_config_directory() {
    let config = tempfile::tempdir().unwrap();
    std::fs::write(
        config.path().join("settings.json"),
        r#"{"promptCacheTtl": "5m"}"#,
    )
    .unwrap();
    let directory = config.path().to_str().unwrap();
    assert_eq!(
        lifetime_with(&[("CLAUDE_CONFIG_DIR", directory)], None),
        FIVE_MINUTES
    );
}
