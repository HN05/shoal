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
    assert_eq!(claude.upstream.as_deref(), Some("http://127.0.0.1:8317"));
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
    let upstream = upstream(kind, proxy, &command, home, home, configured).unwrap();
    let key = match &proxy.key {
        Some(key) => Some(key.resolve(|_| None).await.unwrap()),
        None => None,
    };
    let running = crate::proxy::start(&upstream, key.as_deref())
        .await
        .unwrap();
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

/// Where an agent with integration `kind` launched as `args` in `home`
/// sends its requests without a proxy, with only `variables` set.
fn default_upstream(
    kind: IntegrationKind,
    args: &[&str],
    home: &Path,
    variables: &[(&str, &str)],
) -> String {
    let configured = variables
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).into()))
        .collect();
    let environment = AgentEnvironment {
        home,
        configured: &configured,
        inherited: |_| None,
    };
    let args: Vec<OsString> = args.iter().map(Into::into).collect();
    integration(kind)
        .default_upstream(&args, home, &environment)
        .unwrap()
}

#[test]
fn claude_goes_to_the_base_url_its_settings_or_environment_name_else_anthropics() {
    let home = tempfile::tempdir().unwrap();
    let url = |args: &[&str], variables: &[(&str, &str)]| {
        default_upstream(IntegrationKind::Claude, args, home.path(), variables)
    };
    let set = [("ANTHROPIC_BASE_URL", "http://environment")];
    assert_eq!(url(&["claude"], &[]), "https://api.anthropic.com");
    assert_eq!(url(&["claude"], &set), "http://environment");
    let user = r#"{"env":{"ANTHROPIC_BASE_URL":"http://user"}}"#;
    fs::create_dir(home.path().join(".claude")).unwrap();
    fs::write(home.path().join(".claude/settings.json"), user).unwrap();
    assert_eq!(url(&["claude"], &set), "http://user");
    let given = r#"{"env":{"ANTHROPIC_BASE_URL":"http://given"}}"#;
    assert_eq!(url(&["claude", "--settings", given], &set), "http://given");
}

#[test]
fn codex_goes_to_its_providers_base_url_else_the_one_its_login_uses() {
    let home = tempfile::tempdir().unwrap();
    let codex_home = home.path().join(".codex");
    fs::create_dir(&codex_home).unwrap();
    let url = |args: &[&str], variables: &[(&str, &str)]| {
        default_upstream(IntegrationKind::Codex, args, home.path(), variables)
    };
    let login = |auth: &str| fs::write(codex_home.join("auth.json"), auth).unwrap();
    let chatgpt = "https://chatgpt.com/backend-api/codex";
    let api = "https://api.openai.com/v1";
    assert_eq!(url(&["codex"], &[]), chatgpt);
    login(r#"{"auth_mode":"chatgpt","OPENAI_API_KEY":null,"tokens":{}}"#);
    assert_eq!(url(&["codex"], &[]), chatgpt);
    login(r#"{"auth_mode":"apikey","OPENAI_API_KEY":"sk-test"}"#);
    assert_eq!(url(&["codex"], &[]), api);
    // A file from before Codex recorded the mode.
    login(r#"{"OPENAI_API_KEY":"sk-test"}"#);
    assert_eq!(url(&["codex"], &[]), api);
    login(r#"{"OPENAI_API_KEY":null,"tokens":{}}"#);
    assert_eq!(url(&["codex"], &[]), chatgpt);
    let moved = [("OPENAI_BASE_URL", "http://moved/v1")];
    assert_eq!(url(&["codex"], &moved), "http://moved/v1");
    // A provider of the user's goes to its own base URL, else the login's.
    fs::write(
        codex_home.join("config.toml"),
        "[model_providers.gateway]\nname = 'Gateway'\nbase_url = 'http://gateway/v1'\n\
         [model_providers.bare]\nname = 'Bare'\n",
    )
    .unwrap();
    let gateway = ["codex", "-c", "model_provider=gateway"];
    assert_eq!(url(&gateway, &moved), "http://gateway/v1");
    assert_eq!(url(&["codex", "-c", "model_provider=bare"], &[]), chatgpt);
}
