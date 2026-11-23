use anyhow::Result;

use super::*;
use crate::config::proxies::{routes, validate, validate_references};

fn parse(text: &str) -> Result<crate::config::Config> {
    let config = crate::config::Config::from_toml(text)?;
    validate(&config.machine.proxies)?;
    validate_references(&config.machine.agents, &config.machine.proxies)?;
    Ok(config)
}

#[test]
fn tools_and_their_profiles_reach_the_upstream_of_the_proxy_they_name() {
    let config = parse(
        "[machine.proxies.cliproxy]\nupstream = 'http://127.0.0.1:8317'\n\
         \n[agents.claude-fast]\nbase = 'claude'\n[machine.agents.claude]\nproxy = 'cliproxy'",
    )
    .unwrap();
    let routes = routes(&config.tools().unwrap(), &config.machine.proxies);
    let upstream = |name: &str| routes[name].upstream.clone();
    assert_eq!(upstream("claude").as_deref(), Some("http://127.0.0.1:8317"));
    assert_eq!(
        upstream("claude-fast").as_deref(),
        Some("http://127.0.0.1:8317")
    );
    // A proxy without an upstream only retries.
    assert!(parse("[machine.proxies.retry]\n[machine.agents.codex]\nproxy = 'retry'\n").is_ok());
    assert!(!routes.contains_key("codex"));
    for text in [
        "[machine.agents.claude]\nproxy = 'missing'\n",
        "[machine.proxies.local]\nupstream = 'ftp://host'\n",
        "[machine.proxies.local]\nkey = 'sk-gateway'\n",
        "[machine.proxies.local]\nupstream = 'http://host'\nkey = 'two words'\n",
        "[machine.proxies.local]\nupstream = 'http://host'\nkey = { env = 'A-B' }\n",
        "[machine.proxies.local]\nupstream = 'http://host'\nkey = { cmd = [] }\n",
        "[machine.proxies.local]\nupstream = 'http://host'\nkey = { file = 'key' }\n",
    ] {
        assert!(parse(text).is_err(), "{text}");
    }
}

#[test]
fn an_upstream_the_proxy_cannot_reach_is_refused_by_its_key() {
    for upstream in ["https://", "https://h/?q=1"] {
        let text = format!("[machine.proxies.local]\nupstream = '{upstream}'\n");
        let error = parse(&text).unwrap_err().to_string();
        assert!(error.contains("machine.proxies.local.upstream"), "{error}");
    }
}

#[test]
fn a_proxy_codex_names_takes_a_server_root_rather_than_codexs_api_url() {
    let with = |upstream: &str, agent: &str| {
        let text = format!(
            "[machine.proxies.p]\nupstream = '{upstream}'\n\
             [agents.codex-fast]\nbase = 'codex'\n[machine.agents.{agent}]\nproxy = 'p'\n"
        );
        parse(&text).unwrap().resolve(&Default::default())
    };
    for upstream in [
        "http://127.0.0.1:8317/v1",
        "http://127.0.0.1:8317/v1/",
        "https://api.openai.com/v1",
        "https://chatgpt.com/backend-api/codex",
    ] {
        for agent in ["codex", "codex-fast"] {
            let error = with(upstream, agent).unwrap_err().to_string();
            assert!(error.contains("machine.proxies.p.upstream"), "{error}");
        }
        // Claude names the version in its requests' own paths.
        assert!(with(upstream, "claude").is_ok(), "{upstream}");
    }
    for upstream in ["http://127.0.0.1:8317", "https://gateway.example/api/v10"] {
        assert!(with(upstream, "codex").is_ok(), "{upstream}");
    }
}

#[tokio::test]
async fn a_proxy_key_is_written_out_read_from_the_agents_environment_or_fetched() {
    let config = parse(
        "[machine.proxies.raw]\nupstream = 'http://host'\nkey = 'sk-raw'\n\
         [machine.proxies.env]\nupstream = 'http://host'\nkey = { env = 'CLIPROXY_KEY' }\n\
         [machine.proxies.cmd]\nupstream = 'http://host'\nkey = { cmd = ['echo', 'sk-cmd'] }\n",
    )
    .unwrap();
    let key = |name: &str| config.machine.proxies[name].key.clone().unwrap();
    let set = |name: &str| (name == "CLIPROXY_KEY").then(|| "sk-env".into());
    assert_eq!(
        crate::config::value::resolve(&key("raw"), set)
            .await
            .unwrap(),
        "sk-raw"
    );
    assert_eq!(
        crate::config::value::resolve(&key("env"), set)
            .await
            .unwrap(),
        "sk-env"
    );
    assert_eq!(
        crate::config::value::resolve(&key("cmd"), set)
            .await
            .unwrap(),
        "sk-cmd"
    );
    // A key as written never shows in what Shoal prints.
    let printed = format!("{:?}", config.machine.proxies["raw"]);
    assert!(!printed.contains("sk-raw"), "{printed}");
}

#[test]
fn agents_name_the_key_in_the_header_the_proxy_reads() {
    assert_eq!(KEY_HEADER, shoal_proxy::CLIENT_KEY_HEADER);
}
