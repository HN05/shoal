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
    let routes = routes(&config.ai.tools, &config.proxy);
    assert_eq!(routes["claude"].upstream, "http://127.0.0.1:8317");
    assert_eq!(routes["claude-fast"].upstream, "http://127.0.0.1:8317");
    assert!(!routes.contains_key("codex"));
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
        (
            "[proxy.local]\nupstream = 'http://host'\nkey = 'two words'\n",
            "proxy.local.key must be visible ASCII",
        ),
        (
            "[proxy.local]\nupstream = 'http://host'\nkey = { env = 'A-B' }\n",
            "proxy.local.key",
        ),
        (
            "[proxy.local]\nupstream = 'http://host'\nkey = { file = 'key' }\n",
            "naming the variable that holds it",
        ),
    ] {
        let error = format!("{:#}", parse(text).unwrap_err());
        assert!(error.contains(message), "{text}: {error}");
    }
}

#[test]
fn a_proxy_key_is_written_out_or_read_from_the_agents_environment() {
    let config = parse(
        "[proxy.raw]\nupstream = 'http://host'\nkey = 'sk-raw'\n\
         [proxy.env]\nupstream = 'http://host'\nkey = { env = 'CLIPROXY_KEY' }\n",
    )
    .unwrap();
    let key = |name: &str| config.proxy[name].key.clone().unwrap();
    let set = |name: &str| (name == "CLIPROXY_KEY").then(|| "sk-env".into());
    assert_eq!(key("raw").value(set).unwrap(), "sk-raw");
    assert_eq!(key("env").value(set).unwrap(), "sk-env");
    let error = format!("{:#}", key("env").value(|_| None).unwrap_err());
    assert!(error.contains("CLIPROXY_KEY is not set"), "{error}");
    // A key as written never shows in what Shoal prints.
    let printed = format!("{:?}", config.proxy["raw"]);
    assert!(!printed.contains("sk-raw"), "{printed}");
}
