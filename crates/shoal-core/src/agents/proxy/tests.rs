use std::fs;

use super::*;
use crate::agents::ensure_available;

#[test]
fn a_configured_resume_command_still_goes_through_the_agents_proxy() {
    let config = crate::config::Config::from_toml(
        "[machine.proxies.local]\nupstream = 'http://127.0.0.1:8317'\n\
         \n[machine.agents.claude]\nproxy = 'local'\n[machine.agents.codex]\nproxy = 'local'",
    )
    .unwrap();
    let settings = config.resolve(&Default::default()).unwrap();
    let with = |name: &str, command: &[&str]| {
        let mut command: Vec<OsString> = command.iter().map(Into::into).collect();
        with_proxy_args(&settings, &name.into(), &mut command);
        command
    };
    // Claude gets settings of its own for the wrapper's merge to fill in.
    let claude = with("claude", &["claude", "--continue"]);
    assert_eq!(claude, ["claude", "--continue", "--settings", "{}"]);
    // Codex's provider placeholder comes before a prompt after `--`.
    assert_eq!(
        with("codex", &["saved-session", "--", "prompt"]),
        [
            "saved-session",
            "{shoal-proxy-codex-provider}",
            "--",
            "prompt"
        ]
    );
    // Then the wrapper routes them as it does a launch's own.
    let home = tempfile::tempdir().unwrap();
    let routing = Routing {
        url: "http://127.0.0.1:4000",
        directory: home.path(),
        private: home.path(),
    };
    let read = |path: &OsString| -> serde_json::Value {
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    };
    let routed = route_through_proxy(
        IntegrationKind::Claude,
        claude,
        &routing,
        home.path(),
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(
        read(&routed[3])["env"]["ANTHROPIC_BASE_URL"],
        "http://127.0.0.1:4000"
    );
    // A resume command's own settings stay, joined with the proxy's.
    let own = with(
        "claude",
        &["claude", "--settings", r#"{"model":"opus"}"#, "--continue"],
    );
    let routed = route_through_proxy(
        IntegrationKind::Claude,
        own,
        &routing,
        home.path(),
        &BTreeMap::new(),
    )
    .unwrap();
    assert_eq!(routed.iter().filter(|arg| *arg == "--settings").count(), 1);
    let joined = read(&routed[2]);
    assert_eq!(joined["model"], "opus", "{joined}");
    assert_eq!(joined["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:4000");
    // An agent without a proxy runs its command as configured.
    let plain = crate::config::Config::from_toml("").unwrap();
    let plain = plain.resolve(&Default::default()).unwrap();
    let mut command: Vec<OsString> = vec!["claude".into(), "--continue".into()];
    with_proxy_args(&plain, &"claude".into(), &mut command);
    assert_eq!(command, ["claude", "--continue"]);
}

#[test]
fn a_proxy_is_refused_for_an_agent_shoal_cannot_point_at_it() {
    let config = crate::config::Config::from_toml(
        "[machine.proxies.local]\nupstream = 'http://127.0.0.1:8317'\n\
         \n[machine.agents.pi]\nproxy = 'local'\n[machine.agents.claude]\nproxy = 'local'",
    )
    .unwrap();
    let settings = config.resolve(&Default::default()).unwrap();
    assert!(ensure_available(&"pi".into(), &settings).is_err());
    // Every launch, a resumed one included, prepares its proxy this way.
    assert!(launch_proxy(&settings, &"pi".into()).is_err());
    let claude = launch_proxy(&settings, &"claude".into()).unwrap().unwrap();
    assert_eq!(claude.upstream, "http://127.0.0.1:8317");
    assert!(launch_proxy(&settings, &"codex".into()).unwrap().is_none());
    // Claude can; whether it is installed is another matter.
    if let Err(error) = ensure_available(&"claude".into(), &settings) {
        assert!(!error.to_string().contains("proxy"), "{error}");
    }
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
        crate::proxy::KEY_HEADER
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

/// The path at the upstream of a request to `path` from the agent `name`,
/// launched as `command` with Codex's config in `home`, through `running`,
/// whose upstream sends each request line to `received`.
async fn reached(
    settings: &crate::config::Effective,
    running: &crate::proxy::Running,
    received: &mut tokio::sync::mpsc::UnboundedReceiver<String>,
    (name, command, path): (&str, &[&str], &str),
    home: &Path,
) -> String {
    let kind = settings.integration(&name.into());
    let mut command: Vec<OsString> = command.iter().map(Into::into).collect();
    with_proxy_args(settings, &name.into(), &mut command);
    let url = agent_url(kind, &running.url);
    let routing = Routing {
        url: &url,
        directory: home,
        private: running.private_dir(),
    };
    let configured = BTreeMap::from([("CODEX_HOME".to_owned(), home.into())]);
    let routed = route_through_proxy(kind, command, &routing, home, &configured).unwrap();
    post(&format!("{}{path}", base_url(&routed)), &running.key).await;
    let line = received.recv().await.unwrap();
    line.split(' ').nth(1).unwrap().to_owned()
}

#[tokio::test]
async fn claude_and_codex_reach_their_apis_under_the_upstream_of_one_proxy() {
    let (upstream, mut received) = fake_upstream().await;
    let config = crate::config::Config::from_toml(&format!(
        "[machine.proxies.cliproxy]\nupstream = '{upstream}'\nkey = 'sk-gateway'\n\
         \n[machine.agents.claude]\nproxy = 'cliproxy'\n[machine.agents.codex]\nproxy = 'cliproxy'"
    ))
    .unwrap();
    let settings = config.resolve(&Default::default()).unwrap();
    let proxy = launch_proxy(&settings, &"codex".into()).unwrap().unwrap();
    let running = crate::proxy::start(proxy, Some("sk-gateway"))
        .await
        .unwrap();
    let home = tempfile::tempdir().unwrap();
    fs::write(
        home.path().join("config.toml"),
        "[model_providers.gateway]\nname = 'Gateway'\nbase_url = 'http://127.0.0.1:9/v1'\n",
    )
    .unwrap();
    let mut reach =
        async |launch| reached(&settings, &running, &mut received, launch, home.path()).await;
    // Claude names the API's version in each request, Codex in its base URL,
    // on the built-in OpenAI provider and on one of the user's.
    let claude = ("claude", &["claude"][..], "/v1/messages");
    assert_eq!(reach(claude).await, "/v1/messages");
    let codex = ("codex", &["codex"][..], "/responses");
    assert_eq!(reach(codex).await, "/v1/responses");
    let gateway = (
        "codex",
        &["codex", "-c", "model_provider=gateway"][..],
        "/responses",
    );
    assert_eq!(reach(gateway).await, "/v1/responses");
}
