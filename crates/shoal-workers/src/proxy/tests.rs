use std::{collections::BTreeMap, ffi::OsString, fs, path::Path};

use crate::agents::{
    Routing, agent_url, launch_proxy, route_through_proxy, upstream, with_proxy_args,
};
use crate::time::UnixTime;

use super::*;

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
async fn only_an_agent_without_a_failure_hook_has_its_turn_ended_by_the_proxy() {
    use shoal_proxy::{Event, Failure};
    // No retry was shown, so none is taken away.
    let failed = ["report_upstream_reset", "report_upstream_limit"];
    for (ends_turns, subagent, expected) in [
        // Claude reports its turn's end itself.
        (false, false, &failed[..]),
        (
            true,
            false,
            &[
                "report_upstream_reset",
                "report_upstream_limit",
                "report_turn_failure",
            ][..],
        ),
        // A Codex subagent's failure leaves its parent's turn going.
        (true, true, &failed[..]),
    ] {
        let root = tempfile::tempdir().unwrap();
        let paths = crate::client::connection::CommandPaths::from(crate::paths::Paths::for_test(
            root.path(),
        ));
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
        let reporter = Reporter {
            paths: paths.clone(),
            workspace: "w".into(),
            execution: "e".into(),
            ends_turns,
            sent: Default::default(),
            now: || UnixTime::from_seconds(1_700_000_000),
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
    let paths =
        crate::client::connection::CommandPaths::from(crate::paths::Paths::for_test(root.path()));
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
    let reporter = Reporter {
        paths: paths.clone(),
        workspace: "w".into(),
        execution: "e".into(),
        ends_turns: true,
        sent: Default::default(),
        now: || UnixTime::from_seconds(1_700_000_000),
    };
    reporter.run(events).await;
    assert_eq!(*methods.lock().unwrap(), ["report_turn_failure"]);
}

#[tokio::test]
async fn a_report_the_upgraded_daemon_refuses_goes_through_the_installed_release() {
    let root = tempfile::tempdir().unwrap();
    let paths =
        crate::client::connection::CommandPaths::from(crate::paths::Paths::for_test(root.path()));
    let binary = root.path().join("shoal");
    let received = root.path().join("received");
    // A new file each time, as an upgrade puts one there.
    let install = |script: &str| crate::test_support::install_test_script(&binary, script);
    install("#!/bin/sh\nexit 1\n");
    // The binary the wrapper started from, read before the upgrade.
    let origin = Origin::at(crate::installed::Executable::at(binary.clone()));
    let report = || crate::protocol::AgentMethod::ReportUpstreamReset {
        workspace: "w".into(),
        execution: "e".into(),
        resets_at: None,
    };
    let (daemon, _) = daemon_speaking(&paths, crate::protocol::VERSION + 1);
    // Before the upgrade replaced it, there is nothing to hand it to.
    send_from(&paths, &origin, report()).await;
    assert!(!received.exists());
    install(&format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n",
        received.display()
    ));
    send_from(&paths, &origin, report()).await;
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
    send_from(&paths, &origin, report()).await;
    assert!(received.exists());
    // A wrapper the daemon never refused needs no relay.
    std::fs::remove_file(&received).unwrap();
    let origin = Origin::at(crate::installed::Executable::at(binary.clone()));
    let _daemon = daemon_speaking(&paths, crate::protocol::VERSION);
    send_from(&paths, &origin, report()).await;
    assert!(!received.exists());
}

#[tokio::test]
async fn the_proxy_report_worker_sends_only_a_wrappers_reports() {
    let root = tempfile::tempdir().unwrap();
    let paths =
        crate::client::connection::CommandPaths::from(crate::paths::Paths::for_test(root.path()));
    for report in [
        r#"{"set_agent_state":{"workspace":"w","state":"idle"}}"#,
        r#"{"report_turn_failure":{"workspace":"w","failure":"overloaded"}}"#,
    ] {
        assert!(send_relayed(&paths, report).await.is_err(), "{report}");
    }
    assert!(send_relayed(&paths, "not json").await.is_err());
    // After an upgrade, the keys an agent reads reach the daemon this way
    // too.
    let (_daemon, methods) = daemon_speaking(&paths, crate::protocol::VERSION);
    let input = r#"{"report_input":{"workspace":"w","execution":"e","at":1700000000}}"#;
    assert_eq!(send_relayed(&paths, input).await.unwrap(), 0);
    assert_eq!(*methods.lock().unwrap(), ["report_input"]);
}

#[tokio::test]
async fn the_reporter_sends_each_change_once_and_clears_only_what_it_reported() {
    use shoal_proxy::{Event, Failure};
    let root = tempfile::tempdir().unwrap();
    let paths =
        crate::client::connection::CommandPaths::from(crate::paths::Paths::for_test(root.path()));
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
        Event::Answered,
        // Nothing is left to clear.
        Event::Answered,
    ] {
        sender.send(event).unwrap();
    }
    drop(sender);
    let reporter = Reporter {
        paths: paths.clone(),
        workspace: "w".into(),
        execution: "e".into(),
        ends_turns: true,
        sent: Default::default(),
        now: || UnixTime::from_seconds(1_700_000_000),
    };
    reporter.run(events).await;
    // The failure took the retry away, so the answer clears the reset
    // and, by reporting the answer, the limit alone.
    assert_eq!(
        *methods.lock().unwrap(),
        [
            "report_proxy_retry",
            "report_proxy_retry",
            "report_upstream_reset",
            "report_upstream_limit",
            "report_upstream_reset",
            "report_upstream_answered",
        ]
    );
}

#[tokio::test]
async fn a_rate_limit_or_spent_quota_is_reported_once_until_the_upstream_answers() {
    use shoal_proxy::{Event, Failure};
    let root = tempfile::tempdir().unwrap();
    let paths =
        crate::client::connection::CommandPaths::from(crate::paths::Paths::for_test(root.path()));
    let (_daemon, methods) = daemon_speaking(&paths, crate::protocol::VERSION);
    let (sender, events) = tokio::sync::mpsc::unbounded_channel();
    let retrying = |attempt| Event::Retrying {
        failure: Failure::RateLimited,
        attempt,
        wait: std::time::Duration::from_secs(1),
    };
    let spent = Event::Failed {
        failure: Failure::Exhausted,
        in_stream: false,
        wait: None,
        subagent: false,
    };
    for event in [retrying(1), retrying(2), spent, Event::Answered] {
        sender.send(event).unwrap();
    }
    drop(sender);
    let reporter = Reporter {
        paths: paths.clone(),
        workspace: "w".into(),
        execution: "e".into(),
        ends_turns: false,
        sent: Default::default(),
        now: || UnixTime::from_seconds(1_700_000_000),
    };
    reporter.run(events).await;
    // Retries of the rate limit report it once; the spent quota replaces it.
    assert_eq!(
        *methods.lock().unwrap(),
        [
            "report_upstream_limit",
            "report_proxy_retry",
            "report_proxy_retry",
            "report_proxy_retry",
            "report_upstream_limit",
            "report_upstream_answered",
        ]
    );
}

#[tokio::test]
async fn a_limit_the_proxy_keeps_meeting_is_sent_again_after_a_while() {
    use shoal_proxy::{Event, Failure};
    use std::sync::atomic::{AtomicI64, Ordering};
    static CLOCK: AtomicI64 = AtomicI64::new(1_700_000_000);
    let root = tempfile::tempdir().unwrap();
    let paths =
        crate::client::connection::CommandPaths::from(crate::paths::Paths::for_test(root.path()));
    let (_daemon, methods) = daemon_speaking(&paths, crate::protocol::VERSION);
    let (sender, events) = tokio::sync::mpsc::unbounded_channel();
    let spent = Event::Failed {
        failure: Failure::Exhausted,
        in_stream: false,
        wait: None,
        subagent: false,
    };
    for event in [spent.clone(), spent] {
        sender.send(event).unwrap();
    }
    drop(sender);
    let reporter = Reporter {
        paths: paths.clone(),
        workspace: "w".into(),
        execution: "e".into(),
        ends_turns: false,
        sent: Default::default(),
        // Each reading is a minute and a second after the last.
        now: || UnixTime::from_seconds(CLOCK.fetch_add(61, Ordering::Relaxed)),
    };
    reporter.run(events).await;
    assert_eq!(
        *methods.lock().unwrap(),
        ["report_upstream_limit", "report_upstream_limit"]
    );
}

#[tokio::test]
async fn answers_are_reported_once_a_minute_or_at_once_after_a_limit() {
    use shoal_proxy::{Event, Failure};
    use std::sync::atomic::{AtomicI64, Ordering};
    static CLOCK: AtomicI64 = AtomicI64::new(1_700_000_000);
    let root = tempfile::tempdir().unwrap();
    let paths =
        crate::client::connection::CommandPaths::from(crate::paths::Paths::for_test(root.path()));
    let (_daemon, methods) = daemon_speaking(&paths, crate::protocol::VERSION);
    let (sender, events) = tokio::sync::mpsc::unbounded_channel();
    let spent = Event::Failed {
        failure: Failure::Exhausted,
        in_stream: false,
        wait: None,
        subagent: false,
    };
    for event in [
        Event::Answered,
        Event::Answered,
        spent,
        Event::Answered,
        Event::Answered,
    ] {
        sender.send(event).unwrap();
    }
    drop(sender);
    let reporter = Reporter {
        paths: paths.clone(),
        workspace: "w".into(),
        execution: "e".into(),
        ends_turns: false,
        sent: Default::default(),
        // Each reading is half a minute after the last.
        now: || UnixTime::from_seconds(CLOCK.fetch_add(30, Ordering::Relaxed)),
    };
    reporter.run(events).await;
    assert_eq!(
        *methods.lock().unwrap(),
        [
            "report_upstream_answered",
            "report_upstream_limit",
            "report_upstream_answered",
        ]
    );
}

#[tokio::test]
async fn subagents_requests_are_reported_at_most_once_a_minute() {
    use shoal_proxy::Event;
    use std::sync::atomic::{AtomicI64, Ordering};
    static CLOCK: AtomicI64 = AtomicI64::new(1_700_000_000);
    let root = tempfile::tempdir().unwrap();
    let paths =
        crate::client::connection::CommandPaths::from(crate::paths::Paths::for_test(root.path()));
    let (_daemon, methods) = daemon_speaking(&paths, crate::protocol::VERSION);
    let (sender, events) = tokio::sync::mpsc::unbounded_channel();
    for _ in 0..3 {
        sender.send(Event::SubagentRequest).unwrap();
    }
    drop(sender);
    let reporter = Reporter {
        paths: paths.clone(),
        workspace: "w".into(),
        execution: "e".into(),
        ends_turns: false,
        sent: Default::default(),
        // Each reading is half a minute after the last.
        now: || UnixTime::from_seconds(CLOCK.fetch_add(30, Ordering::Relaxed)),
    };
    reporter.run(events).await;
    assert_eq!(
        *methods.lock().unwrap(),
        ["report_subagent_request", "report_subagent_request"]
    );
}

#[tokio::test]
async fn a_report_the_daemon_did_not_take_goes_again_with_the_next_event() {
    use shoal_proxy::{Event, Failure};
    let root = tempfile::tempdir().unwrap();
    let paths =
        crate::client::connection::CommandPaths::from(crate::paths::Paths::for_test(root.path()));
    // The daemon refuses the first reset, as one restarting might.
    let (_daemon, methods) = daemon_refusing(&paths, crate::protocol::VERSION, &[0]);
    let (sender, events) = tokio::sync::mpsc::unbounded_channel();
    let limited = Event::Failed {
        failure: Failure::RateLimited,
        in_stream: false,
        wait: Some(std::time::Duration::from_secs(7200)),
        subagent: false,
    };
    for event in [limited.clone(), limited, Event::Answered] {
        sender.send(event).unwrap();
    }
    drop(sender);
    let reporter = Reporter {
        paths: paths.clone(),
        workspace: "w".into(),
        execution: "e".into(),
        ends_turns: false,
        sent: Default::default(),
        now: || UnixTime::from_seconds(1_700_000_000),
    };
    reporter.run(events).await;
    assert_eq!(
        *methods.lock().unwrap(),
        [
            "refused report_upstream_reset",
            "report_upstream_limit",
            "report_upstream_reset",
            "report_upstream_reset",
            "report_upstream_answered",
        ]
    );
}

#[tokio::test]
async fn a_recovery_the_daemon_took_in_part_keeps_what_it_did_not() {
    use shoal_proxy::{Event, Failure};
    let root = tempfile::tempdir().unwrap();
    let paths =
        crate::client::connection::CommandPaths::from(crate::paths::Paths::for_test(root.path()));
    // The daemon refuses the retry's clearing but takes the reset's.
    let (_daemon, methods) = daemon_refusing(&paths, crate::protocol::VERSION, &[3]);
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
    for event in [limited.clone(), retrying, Event::Answered, limited] {
        sender.send(event).unwrap();
    }
    drop(sender);
    let reporter = Reporter {
        paths: paths.clone(),
        workspace: "w".into(),
        execution: "e".into(),
        ends_turns: false,
        sent: Default::default(),
        now: || UnixTime::from_seconds(1_700_000_000),
    };
    reporter.run(events).await;
    // The reset the daemon dropped goes again, and the retry it still shows
    // is cleared again.
    assert_eq!(
        *methods.lock().unwrap(),
        [
            "report_upstream_reset",
            "report_upstream_limit",
            "report_proxy_retry",
            "refused report_proxy_retry",
            "report_upstream_reset",
            "report_upstream_answered",
            "report_proxy_retry",
            "report_upstream_reset",
            "report_upstream_limit",
        ]
    );
}

/// A server on a loopback port that answers every request with an empty
/// JSON object, and the request line of each, such as
/// `POST /v1/responses HTTP/1.1`.
async fn fake_upstream() -> (String, tokio::sync::mpsc::UnboundedReceiver<String>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (lines, received) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut head = Vec::new();
            let mut byte = [0; 1];
            while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).await.unwrap_or(0) == 1 {
                head.push(byte[0]);
            }
            let head = String::from_utf8_lossy(&head);
            let _ = lines.send(head.lines().next().unwrap_or_default().to_owned());
            let reply = "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                         content-length: 2\r\nconnection: close\r\n\r\n{}";
            let _ = stream.write_all(reply.as_bytes()).await;
        }
    });
    (url, received)
}

/// Send a request to `url`, on a proxy that answers requests naming `key`,
/// and wait for its answer.
async fn post(url: &str, key: &str) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let rest = url.strip_prefix("http://").unwrap();
    let (authority, path) = rest.split_at(rest.find('/').unwrap());
    let mut stream = tokio::net::TcpStream::connect(authority).await.unwrap();
    let request = format!(
        "POST {path} HTTP/1.1\r\nhost: {authority}\r\n{}: {key}\r\n\
         content-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{{}}",
        shoal_core::proxy::KEY_HEADER
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).await.unwrap();
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
}

/// The base URL a routed Claude or Codex `command` sends its requests to.
fn base_url(command: &[OsString]) -> String {
    if let Some(at) = command.iter().position(|arg| arg == "--settings") {
        let settings: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&command[at + 1]).unwrap()).unwrap();
        return settings["env"]["ANTHROPIC_BASE_URL"]
            .as_str()
            .unwrap()
            .to_owned();
    }
    let at = command.iter().rposition(|arg| {
        let arg = arg.to_string_lossy();
        arg.starts_with("model_providers.") && arg.contains("base_url=")
    });
    let config: toml::Table = toml::from_str(&command[at.unwrap()].to_string_lossy()).unwrap();
    let provider = config["model_providers"]
        .as_table()
        .unwrap()
        .values()
        .next();
    provider.unwrap()["base_url"].as_str().unwrap().to_owned()
}

/// How an agent is launched: its name, its command and the path of its
/// request, relative to its base URL.
type Launch<'a> = (&'a str, &'a [&'a str], &'a str);

/// The path at the upstream of a request from `launch` in `home`, running
/// with `configured`, through a proxy started as its wrapper starts one,
/// whose upstream sends each request line to `received`.
async fn reached(
    settings: &crate::config::Effective,
    (name, command, path): Launch<'_>,
    home: &Path,
    configured: &BTreeMap<String, OsString>,
    received: &mut tokio::sync::mpsc::UnboundedReceiver<String>,
) -> String {
    let kind = settings.integration(&name.into());
    let proxy = launch_proxy(settings, &name.into()).unwrap().unwrap();
    let mut command: Vec<OsString> = command.iter().map(Into::into).collect();
    with_proxy_args(settings, &name.into(), &mut command);
    let variable = |name: &str| crate::env::configured_var(configured, name);
    let upstream = upstream(kind, proxy, &command, home, &variable, home).unwrap();
    let key = match &proxy.key {
        Some(key) => Some(key.resolve(|_| None).await.unwrap()),
        None => None,
    };
    let running = super::start(&upstream, key.as_deref()).await.unwrap();
    let url = agent_url(kind, &running.url, proxy);
    let routing = Routing {
        url: &url,
        directory: home,
        private: running.private_dir(),
    };
    let routed = route_through_proxy(kind, command, &routing, home, configured).unwrap();
    post(&format!("{}{path}", base_url(&routed)), &running.key).await;
    let line = received.recv().await.unwrap();
    line.split(' ').nth(1).unwrap().to_owned()
}

/// Settings in which Claude and Codex go through one proxy, with `table`.
fn one_proxy(table: &str) -> crate::config::Effective {
    let config = crate::config::Config::from_toml(&format!(
        "[machine.proxies.cliproxy]\n{table}\n\
         [machine.agents.claude]\nproxy = 'cliproxy'\n[machine.agents.codex]\nproxy = 'cliproxy'"
    ))
    .unwrap();
    config.resolve(&Default::default()).unwrap()
}

/// Claude, Codex on its built-in OpenAI provider, and Codex on a provider
/// of the user's, `gateway`.
const CLAUDE: Launch<'static> = ("claude", &["claude"], "/v1/messages");
const CODEX: Launch<'static> = ("codex", &["codex"], "/responses");
const GATEWAY: Launch<'static> = (
    "codex",
    &["codex", "-c", "model_provider=gateway"],
    "/responses",
);

#[tokio::test]
async fn claude_and_codex_reach_their_apis_under_the_upstream_of_one_proxy() {
    let (upstream, mut received) = fake_upstream().await;
    let settings = one_proxy(&format!("upstream = '{upstream}'\nkey = 'sk-gateway'"));
    let home = tempfile::tempdir().unwrap();
    fs::write(
        home.path().join("config.toml"),
        "[model_providers.gateway]\nname = 'Gateway'\nbase_url = 'http://127.0.0.1:9/v1'\n",
    )
    .unwrap();
    let configured = BTreeMap::from([("CODEX_HOME".to_owned(), home.path().into())]);
    let mut reach =
        async |launch| reached(&settings, launch, home.path(), &configured, &mut received).await;
    // Claude names the API's version in each request, Codex in its base URL,
    // on the built-in OpenAI provider and on one of the user's.
    assert_eq!(reach(CLAUDE).await, "/v1/messages");
    assert_eq!(reach(CODEX).await, "/v1/responses");
    assert_eq!(reach(GATEWAY).await, "/v1/responses");
}

#[tokio::test]
async fn a_proxy_without_an_upstream_forwards_where_each_agent_would_send_its_requests() {
    let (upstream, mut received) = fake_upstream().await;
    let settings = one_proxy("");
    let home = tempfile::tempdir().unwrap();
    fs::write(
        home.path().join("config.toml"),
        format!("[model_providers.gateway]\nname = 'Gateway'\nbase_url = '{upstream}/v1'\n"),
    )
    .unwrap();
    let configured = BTreeMap::from([
        ("CODEX_HOME".to_owned(), home.path().into()),
        ("CLAUDE_CONFIG_DIR".to_owned(), home.path().into()),
        ("ANTHROPIC_BASE_URL".to_owned(), upstream.into()),
    ]);
    let mut reach =
        async |launch| reached(&settings, launch, home.path(), &configured, &mut received).await;
    assert_eq!(reach(CLAUDE).await, "/v1/messages");
    assert_eq!(reach(GATEWAY).await, "/v1/responses");
}
