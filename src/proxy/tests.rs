use anyhow::Result;

use super::*;

fn parse(text: &str) -> Result<crate::config::Config> {
    let config: crate::config::Config = toml::from_str(text)?;
    validate(&config.proxy)?;
    validate_references(&config.ai.entries, &config.proxy)?;
    Ok(config)
}

#[test]
fn the_url_fills_in_wherever_launch_arguments_name_it() {
    let mut args: Vec<std::ffi::OsString> = vec![
        "--settings".into(),
        format!(r#"{{"env":{{"ANTHROPIC_BASE_URL":"{URL}"}}}}"#).into(),
        "plain".into(),
    ];
    fill_url(&mut args, "http://127.0.0.1:4000");
    assert_eq!(
        args[1],
        r#"{"env":{"ANTHROPIC_BASE_URL":"http://127.0.0.1:4000"}}"#
    );
    assert_eq!(args[2], "plain");
}

#[test]
fn tools_and_their_profiles_reach_the_upstream_of_the_proxy_they_name() {
    let config = parse(
        "[proxy.cliproxy]\nupstream = 'http://127.0.0.1:8317'\n\
         [ai.claude]\nproxy = 'cliproxy'\n[ai.claude-fast]\nbase = 'claude'\n",
    )
    .unwrap();
    let upstreams = upstreams(&config.ai.tools, &config.proxy);
    assert_eq!(upstreams["claude"], "http://127.0.0.1:8317");
    assert_eq!(upstreams["claude-fast"], "http://127.0.0.1:8317");
    assert!(!upstreams.contains_key("codex"));
    for (text, message) in [
        (
            "[ai.claude]\nproxy = 'missing'\n",
            "no [proxy.missing] defines",
        ),
        (
            "[proxy.local]\nupstream = 'ftp://host'\n",
            "proxy.local.upstream",
        ),
        ("[proxy.local]\nurl = 'http://host'\n", "unknown field"),
    ] {
        let error = format!("{:#}", parse(text).unwrap_err());
        assert!(error.contains(message), "{text}: {error}");
    }
}
