//! Opening a demo on another machine: the demo's machine relays a stream's
//! bytes to and from the demo's port through the hub, unread.
use std::{io::BufRead as _, path::PathBuf, process::Stdio};

use shoal_client::{
    connection::Daemon,
    demos::{DemoStream, open_stream},
};
use shoal_protocol::{demos::Kind, space_names::SpaceSelector};
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _},
    net::TcpStream,
};

use super::{
    directory::{join, listed, machine, workspace},
    following::app,
    numbers::reroute,
    support::{
        fixture::{Base, home},
        install_test_script, ports,
        wait::wait_for,
        zmx::PASSTHROUGH_ZMX,
    },
};

/// A server on `$PORT` that sends back each connection's bytes as they
/// come, and writes `listening` in its directory once it listens.
const ECHO: &str = r"import os, socket
s = socket.create_server(('127.0.0.1', int(os.environ['PORT'])))
open('listening', 'w').close()
while True:
    c, _ = s.accept()
    while chunk := c.recv(65536):
        c.sendall(chunk)
    c.close()
";

/// A laptop joined to `hub` whose new workspace runs the web demo `site`,
/// [`ECHO`], once it listens; with the workspace's ID and the laptop's.
fn laptop_demo(hub: &Base) -> (Base, String, String) {
    // A port of its own, which no other test's daemon reserves.
    let port = ports::hold().port;
    let config = format!("[ports]\nstart = {port}\nend = {port}\n");
    let laptop = Base::start(home(), &config, "app", &[]);
    let laptop_id = join(&laptop, hub, "laptop");
    install_test_script(laptop.root.path().join("bin/zmx"), PASSTHROUGH_ZMX);
    // Debug builds only open an existing ownership lock of the host.
    std::fs::write(laptop.root.path().join("zmx.shoal-ownership.lock"), "").unwrap();
    let space = workspace(&laptop, "demoed");
    let args = [
        "demo",
        "start",
        "site",
        "--preset",
        "web",
        "--workspace",
        &space,
    ];
    laptop.ok(&[&args[..], &["--", "python3", "-c", ECHO]].concat());
    let path = PathBuf::from(
        laptop.ok(&["status", &space])["workspace"]["path"]
            .as_str()
            .unwrap(),
    );
    wait_for("the demo to listen", || {
        path.join("listening").exists().then_some(())
    });
    listed(hub, &space, &laptop_id);
    (laptop, space, laptop_id)
}

fn selected(space: &str) -> SpaceSelector {
    SpaceSelector::id(&space.into())
}

/// Send `bytes` on `connection` and read them back, as the echo sends
/// them, while it sends.
async fn echoed(connection: impl AsyncRead + AsyncWrite + Unpin, bytes: &[u8]) {
    let (mut reader, mut writer) = tokio::io::split(connection);
    let sent = async {
        writer.write_all(bytes).await.unwrap();
        writer.flush().await.unwrap();
    };
    let mut back = vec![0; bytes.len()];
    let read = reader.read_exact(&mut back);
    let (_, read) = tokio::join!(sent, read);
    read.unwrap();
    assert!(back == bytes, "the echo differs from what was sent");
}

/// Open `space`'s demo `site` from `daemon`.
async fn open(daemon: &impl Daemon, space: &str) -> anyhow::Result<DemoStream> {
    open_stream(daemon, selected(space), "site".into()).await
}

#[test]
fn an_app_opens_a_web_demo_on_another_machine_both_ways() {
    let hub = machine(true, "app");
    let (_laptop, space, _) = laptop_demo(&hub);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let mut stream = open(&app(&hub), &space).await.unwrap();
        assert_eq!(stream.demo.kind, Kind::Web);
        // Turns go both ways on one connection, as a WebSocket's do, and
        // one longer than a stream's credit comes back whole.
        echoed(&mut stream.connection, b"hello").await;
        echoed(&mut stream.connection, &vec![7; 1 << 20]).await;
        stream.connection.get_mut().shutdown().await.unwrap();
        let mut rest = Vec::new();
        stream.connection.read_to_end(&mut rest).await.unwrap();
        assert!(rest.is_empty());
    });
}

#[test]
fn shoal_demo_forwards_another_machine_s_web_demo_from_a_local_port() {
    let hub = machine(true, "app");
    let (_laptop, space, laptop_id) = laptop_demo(&hub);
    let desk = machine(false, "app");
    join(&desk, &hub, "desk");
    listed(&desk, &space, &laptop_id);
    let mut forwarding = desk
        .command()
        .args(["--json", "demo", "site", "--workspace", &space])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut printed = String::new();
    let stdout = forwarding.stdout.take().unwrap();
    std::io::BufReader::new(stdout)
        .read_line(&mut printed)
        .unwrap();
    let printed: serde_json::Value = serde_json::from_str(&printed).unwrap();
    let port = printed["port"].as_u64().unwrap();
    assert_eq!(printed["location"], format!("http://localhost:{port}"));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        // Each connection reaches the demo on its own.
        for turn in [&b"first"[..], b"second"] {
            let connection = TcpStream::connect(("127.0.0.1", port as u16)).await;
            echoed(connection.unwrap(), turn).await;
        }
    });
    forwarding.kill().unwrap();
    forwarding.wait().unwrap();
}

#[test]
fn a_demo_stream_is_refused_to_a_space_whose_scope_lacks_the_demo() {
    let hub = machine(true, "app");
    let (_laptop, space, _) = laptop_demo(&hub);
    let other = workspace(&hub, "other");
    let issued = hub.request(serde_json::json!({"issue_scope": {"space": other}}));
    let token = issued["data"].as_str().unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let refused = runtime.block_on(open(&app(&hub).scope(token.into()), &space));
    let error = format!("{:#}", refused.err().expect("a refusal"));
    assert!(error.contains("scope"), "{error}");
}

#[test]
fn opening_a_demo_on_an_offline_machine_fails_at_once_naming_it() {
    let hub = machine(true, "app");
    let (laptop, space, _) = laptop_demo(&hub);
    reroute(&laptop, Some("false"));
    wait_for("the hub to see the laptop go", || {
        let members = hub.ok(&["hub", "ls"])["members"].clone();
        let laptop = (members.as_array().unwrap().iter()).find(|member| member["name"] == "laptop");
        (laptop?["connected"] == false).then_some(())
    });
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let refused = runtime.block_on(open(&app(&hub), &space));
    let error = format!("{:#}", refused.err().expect("a refusal"));
    assert!(error.contains("machine laptop is offline"), "{error}");
}
