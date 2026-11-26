//! Opening a demo on another machine: the demo's machine relays a stream's
//! bytes to and from the demo's port through the hub, unread, or shows a
//! terminal demo's place on a terminal it makes for the viewer.
use std::{
    fs,
    io::{BufRead as _, Read as _, Write as _},
    path::PathBuf,
    process::Stdio,
};

use shoal_client::{
    connection::Daemon,
    demos::{DemoStream, open_stream, open_terminal},
};
use shoal_protocol::{
    attaching::{FromSession, Terminal, TerminalSize, ToSession},
    demos::Kind,
    space_names::SpaceSelector,
};
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
        install_test_script, ports, printed, pty,
        wait::{try_wait_for, wait_for},
        zmx::PASSTHROUGH_ZMX,
    },
};

/// A zmx whose attach to a session prints `shown` and the session, reads a
/// line typed on its terminal, and writes it, with the terminal's size
/// then, to `typed` in its home; every other call goes to the fake zmx.
const TYPED_ZMX: &str = r#"#!/bin/sh
bin=$(dirname "$0")
if [ "$1" = attach ] && [ $# -eq 2 ]; then
    echo "shown $2"
    read -r line
    echo "$line $(stty size)" > "$bin/../typed"
    exit 0
fi
exec "$bin/zmx-passthrough" "$@"
"#;

/// A laptop joined to `hub` whose new workspace runs the terminal demo
/// `tui` in a session of [`TYPED_ZMX`]; with the workspace's ID and the
/// laptop's.
fn laptop_terminal_demo(hub: &Base) -> (Base, String, String) {
    let laptop = machine(false, "app");
    let laptop_id = join(&laptop, hub, "laptop");
    let bin = laptop.root.path().join("bin");
    install_test_script(bin.join("zmx-passthrough"), PASSTHROUGH_ZMX);
    install_test_script(bin.join("zmx"), TYPED_ZMX);
    // Debug builds only open an existing ownership lock of the host.
    fs::write(laptop.root.path().join("zmx.shoal-ownership.lock"), "").unwrap();
    let space = workspace(&laptop, "demoed");
    let args = ["demo", "start", "tui", "--workspace", &space];
    laptop.ok(&[&args[..], &["--", "sleep", "600"]].concat());
    listed(hub, &space, &laptop_id);
    (laptop, space, laptop_id)
}

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
    let listening = try_wait_for(|| path.join("listening").exists().then_some(()));
    assert!(
        listening.is_some(),
        "timed out waiting for the demo to listen\n{}",
        demo_state(&laptop, &space)
    );
    listed(hub, &space, &laptop_id);
    (laptop, space, laptop_id)
}

/// How `laptop` lists `space`'s demos and what its demo `site` shows, which
/// say why the demo did not listen: a wrapper that ended or restarts, or
/// the command's error.
fn demo_state(laptop: &Base, space: &str) -> String {
    let listing = laptop.run(&["--json", "demo", "ls", "--workspace", space]);
    let screen = laptop.run(&["demo", "screen", "site", "--workspace", space]);
    format!(
        "demo ls: {}\ndemo screen: {}",
        printed(&listing),
        printed(&screen)
    )
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

#[test]
fn a_device_s_stream_shows_a_terminal_demo_on_another_machine_and_types_into_it() {
    let hub = machine(true, "app");
    let (laptop, space, _) = laptop_terminal_demo(&hub);
    let terminal = Terminal {
        size: Some(TerminalSize {
            rows: 24,
            columns: 80,
        }),
        ..Terminal::default()
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (screen, code) = runtime.block_on(async {
        let device = app(&hub);
        let stream = open_terminal(&device, selected(&space), "tui".into(), terminal);
        let mut stream = stream.await.unwrap();
        assert_eq!(stream.demo.kind, Kind::Terminal);
        let mut screen = Vec::new();
        loop {
            let frame = stream.screen.next().await.unwrap();
            match frame.expect("the demo's exit code") {
                FromSession::Screen(bytes) => {
                    let shown_before = String::from_utf8_lossy(&screen).contains("shown");
                    screen.extend(bytes);
                    if !shown_before && String::from_utf8_lossy(&screen).contains("shown") {
                        let size = TerminalSize {
                            rows: 30,
                            columns: 100,
                        };
                        let keys = &mut stream.keys;
                        keys.send(&ToSession::Resize(size)).await.unwrap();
                        keys.send(&ToSession::Keys(b"hello\r".to_vec()))
                            .await
                            .unwrap();
                    }
                }
                FromSession::Exited(code) => {
                    return (String::from_utf8_lossy(&screen).into_owned(), code);
                }
                frame => panic!("a demo's stream sent {frame:?}"),
            }
        }
    });
    assert_eq!(code, 0, "{screen}");
    // The demo's own session, which zmx names after the workspace.
    assert!(screen.contains("shown demoed-tui"), "{screen}");
    let typed = fs::read_to_string(laptop.root.path().join("typed")).unwrap();
    assert_eq!(typed.trim(), "hello 30 100");
}

#[test]
fn shoal_demo_shows_another_machine_s_terminal_demo_in_this_terminal() {
    let hub = machine(true, "app");
    let (laptop, space, laptop_id) = laptop_terminal_demo(&hub);
    let desk = machine(false, "app");
    join(&desk, &hub, "desk");
    listed(&desk, &space, &laptop_id);
    let (mut master, terminal) = pty::open();
    let mut opening = (desk.command())
        .args(["demo", "tui", "--workspace", &space])
        .stdin(terminal.try_clone().unwrap())
        .stdout(terminal.try_clone().unwrap())
        .stderr(terminal.try_clone().unwrap())
        .spawn()
        .unwrap();
    let mut shown = Vec::new();
    wait_for("the demo shown", || {
        read_shown(&mut master, &mut shown);
        String::from_utf8_lossy(&shown)
            .contains("shown demoed-tui")
            .then_some(())
    });
    master.write_all(b"hello\r").unwrap();
    let status = wait_for("the demo's client to exit", || {
        read_shown(&mut master, &mut shown);
        opening.try_wait().unwrap()
    });
    let shown = String::from_utf8_lossy(&shown);
    assert!(status.success(), "{status}: {shown}");
    let typed = fs::read_to_string(laptop.root.path().join("typed")).unwrap();
    assert!(typed.starts_with("hello "), "{typed}");
}

/// Add what `master`'s terminal shows by now to `shown`.
fn read_shown(master: &mut fs::File, shown: &mut Vec<u8>) {
    let mut bytes = [0; 4096];
    while let Ok(read @ 1..) = master.read(&mut bytes) {
        shown.extend_from_slice(&bytes[..read]);
    }
}
