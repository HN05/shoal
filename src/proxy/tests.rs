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

/// A daemon at `paths` that answers each request with protocol `version`,
/// as an upgraded one does to an older client, and keeps the name of each
/// request's method.
fn daemon_speaking(
    paths: &crate::paths::Paths,
    version: u32,
) -> (
    tokio::task::JoinHandle<()>,
    std::sync::Arc<std::sync::Mutex<Vec<String>>>,
) {
    std::fs::create_dir_all(paths.socket.parent().unwrap()).unwrap();
    let listener = tokio::net::UnixListener::bind(&paths.socket).unwrap();
    let methods = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let received = methods.clone();
    let serving = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let (read, mut write) = stream.into_split();
            let mut read = tokio::io::BufReader::new(read);
            let request: serde_json::Value =
                crate::protocol::read_buffered(&mut read).await.unwrap();
            let method = &request["method"];
            let name = method
                .as_object()
                .and_then(|method| method.keys().next().cloned())
                .unwrap_or_else(|| method.as_str().unwrap_or_default().to_owned());
            received.lock().unwrap().push(name);
            let id = request["id"].as_u64().unwrap();
            let mut response = crate::protocol::Response::new(id, crate::protocol::Body::Ok);
            response.protocol = version;
            crate::protocol::write(&mut write, &response).await.unwrap();
        }
    });
    (serving, methods)
}

#[tokio::test]
#[cfg(feature = "proxy")]
async fn only_an_agent_without_a_failure_hook_has_its_turn_ended_by_the_proxy() {
    use shoal_proxy::{Event, Failure};
    for (ends_turns, expected) in [
        // Claude reports its turn's end itself.
        (false, &["report_proxy_retry", "report_upstream_reset"][..]),
        (
            true,
            &[
                "report_proxy_retry",
                "report_upstream_reset",
                "report_turn_failure",
            ][..],
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let paths = crate::paths::Paths::for_test(root.path());
        let (_daemon, methods) = daemon_speaking(&paths, crate::protocol::VERSION);
        let (sender, events) = tokio::sync::mpsc::unbounded_channel();
        sender
            .send(Event::Failed {
                failure: Failure::RateLimited,
                in_stream: true,
                wait: Some(std::time::Duration::from_secs(60)),
            })
            .unwrap();
        drop(sender);
        let reporter = launch::Reporter {
            paths: paths.clone(),
            workspace: "w".into(),
            execution: "e".into(),
            ends_turns,
        };
        reporter.run(events).await;
        assert_eq!(*methods.lock().unwrap(), expected, "{ends_turns}");
    }
    use crate::ai::IntegrationKind;
    assert!(!crate::cli::agents::proxy_ends_turns(
        IntegrationKind::Claude
    ));
    assert!(crate::cli::agents::proxy_ends_turns(IntegrationKind::Codex));
}

#[tokio::test]
async fn a_report_the_upgraded_daemon_refuses_goes_through_the_installed_release() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let paths = crate::paths::Paths::for_test(root.path());
    let binary = root.path().join("shoal");
    let received = root.path().join("received");
    let install = |script: &str| {
        // A new file, as an upgrade puts one there.
        let staged = root.path().join("staged");
        std::fs::write(&staged, script).unwrap();
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::rename(&staged, &binary).unwrap();
    };
    install("#!/bin/sh\nexit 1\n");
    // The binary the wrapper started from, read before the upgrade.
    let executable = crate::cli::installed::Executable::at(binary.clone());
    let report = || crate::protocol::AgentMethod::ReportUpstreamReset {
        workspace: "w".into(),
        execution: "e".into(),
        resets_at: None,
    };
    let (daemon, _) = daemon_speaking(&paths, crate::protocol::VERSION + 1);
    // Before the upgrade replaced it, there is nothing to hand it to.
    launch::send_with(&paths, &executable, report()).await;
    assert!(!received.exists());
    install(&format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n",
        received.display()
    ));
    launch::send_with(&paths, &executable, report()).await;
    let args = std::fs::read_to_string(&received).unwrap();
    let args: Vec<_> = args.lines().collect();
    assert_eq!(
        args[args.len() - 3..args.len() - 1],
        ["internal", "proxy-report"]
    );
    let relayed: serde_json::Value = serde_json::from_str(args[args.len() - 1]).unwrap();
    assert_eq!(relayed["report_upstream_reset"]["execution"], "e");
    // A daemon that accepts the report needs no relay.
    std::fs::remove_file(&received).unwrap();
    daemon.abort();
    std::fs::remove_file(&paths.socket).unwrap();
    let _daemon = daemon_speaking(&paths, crate::protocol::VERSION);
    launch::send_with(&paths, &executable, report()).await;
    assert!(!received.exists());
}

#[tokio::test]
async fn the_proxy_report_worker_sends_only_a_proxys_reports() {
    let root = tempfile::tempdir().unwrap();
    let paths = crate::paths::Paths::for_test(root.path());
    for report in [
        r#"{"set_agent_state":{"workspace":"w","state":"idle"}}"#,
        r#"{"report_turn_failure":{"workspace":"w","failure":"overloaded"}}"#,
    ] {
        let error = send_relayed(&paths, report).await.unwrap_err();
        assert!(
            format!("{error:#}").contains("only a proxy's reports"),
            "{error:#}"
        );
    }
    assert!(send_relayed(&paths, "not json").await.is_err());
}
