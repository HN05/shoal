use anyhow::Result;

use super::*;

fn parse(text: &str) -> Result<crate::config::Config> {
    let config: crate::config::Config = toml::from_str(text)?;
    validate(&config.proxy)?;
    validate_references(&config.ai.entries, &config.proxy)?;
    Ok(config)
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
            "[proxy.local]\nupstream = 'http://host'\nkey = { cmd = [] }\n",
            "proxy.local.key.cmd must name a program",
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

#[tokio::test]
async fn a_proxy_key_is_written_out_read_from_the_agents_environment_or_fetched() {
    let config = parse(
        "[proxy.raw]\nupstream = 'http://host'\nkey = 'sk-raw'\n\
         [proxy.env]\nupstream = 'http://host'\nkey = { env = 'CLIPROXY_KEY' }\n\
         [proxy.cmd]\nupstream = 'http://host'\nkey = { cmd = ['echo', 'sk-cmd'] }\n",
    )
    .unwrap();
    let key = |name: &str| config.proxy[name].key.clone().unwrap();
    let set = |name: &str| (name == "CLIPROXY_KEY").then(|| "sk-env".into());
    assert_eq!(key("raw").resolve(set).await.unwrap(), "sk-raw");
    assert_eq!(key("env").resolve(set).await.unwrap(), "sk-env");
    assert_eq!(key("cmd").resolve(set).await.unwrap(), "sk-cmd");
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
    daemon_refusing(paths, version, &[])
}

/// [`daemon_speaking`], refusing the requests at the places `refused`
/// names, counted from 0, with an error, which it keeps as
/// `refused <method>`.
fn daemon_refusing(
    paths: &crate::paths::Paths,
    version: u32,
    refused: &[usize],
) -> (
    tokio::task::JoinHandle<()>,
    std::sync::Arc<std::sync::Mutex<Vec<String>>>,
) {
    std::fs::create_dir_all(paths.socket.parent().unwrap()).unwrap();
    let listener = tokio::net::UnixListener::bind(&paths.socket).unwrap();
    let methods = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let received = methods.clone();
    let refused = refused.to_vec();
    let serving = tokio::spawn(async move {
        let mut place = 0;
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
            let refuse = refused.contains(&place);
            place += 1;
            let body = if refuse {
                received.lock().unwrap().push(format!("refused {name}"));
                crate::protocol::Body::error(crate::protocol::ErrorCode::OperationFailed, "busy")
            } else {
                received.lock().unwrap().push(name);
                crate::protocol::Body::Ok
            };
            let id = request["id"].as_u64().unwrap();
            let mut response = crate::protocol::Response::new(id, body);
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
    // No retry was shown, so none is taken away.
    let failed = ["report_upstream_reset"];
    for (ends_turns, subagent, expected) in [
        // Claude reports its turn's end itself.
        (false, false, &failed[..]),
        (
            true,
            false,
            &["report_upstream_reset", "report_turn_failure"][..],
        ),
        // A Codex subagent's failure leaves its parent's turn going.
        (true, true, &failed[..]),
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
                subagent,
            })
            .unwrap();
        drop(sender);
        let reporter = launch::Reporter {
            paths: paths.clone(),
            workspace: "w".into(),
            execution: "e".into(),
            ends_turns,
            sent: Default::default(),
            now: || 1_700_000_000,
        };
        reporter.run(events).await;
        assert_eq!(
            *methods.lock().unwrap(),
            expected,
            "{ends_turns} {subagent}"
        );
    }
    // A failure no retry fixes ends a Codex turn too, which the daemon then
    // reports instead of retrying.
    let root = tempfile::tempdir().unwrap();
    let paths = crate::paths::Paths::for_test(root.path());
    let (_daemon, methods) = daemon_speaking(&paths, crate::protocol::VERSION);
    let (sender, events) = tokio::sync::mpsc::unbounded_channel();
    sender
        .send(Event::Failed {
            failure: Failure::Other,
            in_stream: true,
            wait: None,
            subagent: false,
        })
        .unwrap();
    drop(sender);
    let reporter = launch::Reporter {
        paths: paths.clone(),
        workspace: "w".into(),
        execution: "e".into(),
        ends_turns: true,
        sent: Default::default(),
        now: || 1_700_000_000,
    };
    reporter.run(events).await;
    assert_eq!(*methods.lock().unwrap(), ["report_turn_failure"]);
    use crate::ai::IntegrationKind;
    assert!(!crate::cli::agents::proxy_ends_turns(
        IntegrationKind::Claude
    ));
    assert!(crate::cli::agents::proxy_ends_turns(IntegrationKind::Codex));
}

#[tokio::test]
async fn a_report_the_upgraded_daemon_refuses_goes_through_the_installed_release() {
    let root = tempfile::tempdir().unwrap();
    let paths = crate::paths::Paths::for_test(root.path());
    let binary = root.path().join("shoal");
    let received = root.path().join("received");
    // A new file each time, as an upgrade puts one there.
    let install = |script: &str| crate::test_support::install_test_script(&binary, script);
    install("#!/bin/sh\nexit 1\n");
    // The binary the wrapper started from, read before the upgrade.
    let origin = launch::Origin::at(crate::cli::installed::Executable::at(binary.clone()));
    let report = || crate::protocol::AgentMethod::ReportUpstreamReset {
        workspace: "w".into(),
        execution: "e".into(),
        resets_at: None,
    };
    let (daemon, _) = daemon_speaking(&paths, crate::protocol::VERSION + 1);
    // Before the upgrade replaced it, there is nothing to hand it to.
    launch::send_from(&paths, &origin, report()).await;
    assert!(!received.exists());
    install(&format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n",
        received.display()
    ));
    launch::send_from(&paths, &origin, report()).await;
    let args = std::fs::read_to_string(&received).unwrap();
    let args: Vec<_> = args.lines().collect();
    assert_eq!(
        args[args.len() - 3..args.len() - 1],
        ["internal", "proxy-report"]
    );
    let relayed: serde_json::Value = serde_json::from_str(args[args.len() - 1]).unwrap();
    assert_eq!(relayed["report_upstream_reset"]["execution"], "e");
    // Once refused, the wrapper goes to the installed release at once.
    std::fs::remove_file(&received).unwrap();
    daemon.abort();
    std::fs::remove_file(&paths.socket).unwrap();
    launch::send_from(&paths, &origin, report()).await;
    assert!(received.exists());
    // A wrapper the daemon never refused needs no relay.
    std::fs::remove_file(&received).unwrap();
    let origin = launch::Origin::at(crate::cli::installed::Executable::at(binary.clone()));
    let _daemon = daemon_speaking(&paths, crate::protocol::VERSION);
    launch::send_from(&paths, &origin, report()).await;
    assert!(!received.exists());
}

#[tokio::test]
async fn the_proxy_report_worker_sends_only_a_wrappers_reports() {
    let root = tempfile::tempdir().unwrap();
    let paths = crate::paths::Paths::for_test(root.path());
    for report in [
        r#"{"set_agent_state":{"workspace":"w","state":"idle"}}"#,
        r#"{"report_turn_failure":{"workspace":"w","failure":"overloaded"}}"#,
    ] {
        let error = send_relayed(&paths, report).await.unwrap_err();
        assert!(
            format!("{error:#}").contains("only a wrapper's reports"),
            "{error:#}"
        );
    }
    assert!(send_relayed(&paths, "not json").await.is_err());
    // After an upgrade, attachment reports reach the daemon this way too.
    let (_daemon, methods) = daemon_speaking(&paths, crate::protocol::VERSION);
    let attached = r#"{"report_attached":{"workspace":"w","execution":"e","attached":true}}"#;
    assert_eq!(send_relayed(&paths, attached).await.unwrap(), 0);
    assert_eq!(*methods.lock().unwrap(), ["report_attached"]);
}

#[test]
#[cfg(feature = "proxy")]
fn agents_name_the_key_in_the_header_the_proxy_reads() {
    assert_eq!(KEY_HEADER, shoal_proxy::CLIENT_KEY_HEADER);
}

#[tokio::test]
#[cfg(feature = "proxy")]
async fn the_reporter_sends_each_change_once_and_clears_only_what_it_reported() {
    use shoal_proxy::{Event, Failure};
    let root = tempfile::tempdir().unwrap();
    let paths = crate::paths::Paths::for_test(root.path());
    let (_daemon, methods) = daemon_speaking(&paths, crate::protocol::VERSION);
    let (sender, events) = tokio::sync::mpsc::unbounded_channel();
    let limited = Event::Failed {
        failure: Failure::RateLimited,
        in_stream: false,
        wait: Some(std::time::Duration::from_secs(7200)),
        subagent: false,
    };
    for event in [
        Event::Retrying {
            failure: Failure::Overloaded,
            attempt: 1,
            wait: std::time::Duration::from_secs(1),
        },
        limited.clone(),
        // Each request the given-up outage fails names the same reset.
        limited,
        Event::Recovered,
        // Nothing is left to clear.
        Event::Recovered,
    ] {
        sender.send(event).unwrap();
    }
    drop(sender);
    let reporter = launch::Reporter {
        paths: paths.clone(),
        workspace: "w".into(),
        execution: "e".into(),
        ends_turns: true,
        sent: Default::default(),
        now: || 1_700_000_000,
    };
    reporter.run(events).await;
    // The failure took the retry away, so recovery clears the reset alone.
    assert_eq!(
        *methods.lock().unwrap(),
        [
            "report_proxy_retry",
            "report_proxy_retry",
            "report_upstream_reset",
            "report_upstream_reset",
        ]
    );
}

#[tokio::test]
#[cfg(feature = "proxy")]
async fn a_report_the_daemon_did_not_take_goes_again_with_the_next_event() {
    use shoal_proxy::{Event, Failure};
    let root = tempfile::tempdir().unwrap();
    let paths = crate::paths::Paths::for_test(root.path());
    // The daemon refuses the first reset, as one restarting might.
    let (_daemon, methods) = daemon_refusing(&paths, crate::protocol::VERSION, &[0]);
    let (sender, events) = tokio::sync::mpsc::unbounded_channel();
    let limited = Event::Failed {
        failure: Failure::RateLimited,
        in_stream: false,
        wait: Some(std::time::Duration::from_secs(7200)),
        subagent: false,
    };
    for event in [limited.clone(), limited, Event::Recovered] {
        sender.send(event).unwrap();
    }
    drop(sender);
    let reporter = launch::Reporter {
        paths: paths.clone(),
        workspace: "w".into(),
        execution: "e".into(),
        ends_turns: false,
        sent: Default::default(),
        now: || 1_700_000_000,
    };
    reporter.run(events).await;
    assert_eq!(
        *methods.lock().unwrap(),
        [
            "refused report_upstream_reset",
            "report_upstream_reset",
            "report_upstream_reset",
        ]
    );
}

#[tokio::test]
#[cfg(feature = "proxy")]
async fn a_recovery_the_daemon_took_in_part_keeps_what_it_did_not() {
    use shoal_proxy::{Event, Failure};
    let root = tempfile::tempdir().unwrap();
    let paths = crate::paths::Paths::for_test(root.path());
    // The daemon refuses the retry's clearing but takes the reset's.
    let (_daemon, methods) = daemon_refusing(&paths, crate::protocol::VERSION, &[2]);
    let (sender, events) = tokio::sync::mpsc::unbounded_channel();
    let limited = Event::Failed {
        failure: Failure::RateLimited,
        in_stream: false,
        wait: Some(std::time::Duration::from_secs(7200)),
        subagent: false,
    };
    let retrying = Event::Retrying {
        failure: Failure::Overloaded,
        attempt: 1,
        wait: std::time::Duration::from_secs(1),
    };
    for event in [limited.clone(), retrying, Event::Recovered, limited] {
        sender.send(event).unwrap();
    }
    drop(sender);
    let reporter = launch::Reporter {
        paths: paths.clone(),
        workspace: "w".into(),
        execution: "e".into(),
        ends_turns: false,
        sent: Default::default(),
        now: || 1_700_000_000,
    };
    reporter.run(events).await;
    // The reset the daemon dropped goes again, and the retry it still shows
    // is cleared again.
    assert_eq!(
        *methods.lock().unwrap(),
        [
            "report_upstream_reset",
            "report_proxy_retry",
            "refused report_proxy_retry",
            "report_upstream_reset",
            "report_proxy_retry",
            "report_upstream_reset",
        ]
    );
}
