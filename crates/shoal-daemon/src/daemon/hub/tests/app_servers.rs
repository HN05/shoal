use std::{net::SocketAddr, sync::Arc};

use shoal_ids::member_ids::MemberId;
use shoal_protocol::hub::{MemberKind, app_servers};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::Semaphore,
};

use shoal_hub::{
    identity::{Identity, KeyPair, nonce},
    listener, members,
};

use crate::{
    config::Config,
    daemon::{manager::Manager, test_daemon::HANG, test_support},
    time::UnixTime,
};

/// A server that answers each request with `hi`, sending the head of each
/// it got.
async fn server() -> (SocketAddr, tokio::sync::mpsc::UnboundedReceiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (heads, received) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).await.unwrap();
                head.push(byte[0]);
            }
            let _ = heads.send(String::from_utf8(head).unwrap());
            let answer = b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nhi";
            stream.write_all(answer).await.unwrap();
        }
    });
    (address, received)
}

/// The hub forwarding `/quay/` to `server`, with a paired device.
async fn hub(server: SocketAddr) -> (tempfile::TempDir, Arc<Manager>, MemberId, Identity) {
    let (root, manager) = test_support::manager().await;
    forwarding(&manager, server);
    let device = Identity::new(&KeyPair::generate().unwrap()).unwrap();
    let key = device.public.clone();
    let member = (manager.store)
        .write(move |tx| members::join(tx, MemberKind::Device, "phone", &key))
        .await
        .unwrap();
    (root, manager, member, device)
}

/// Make `manager` the hub, forwarding `/quay/` to `server`.
fn forwarding(manager: &Manager, server: SocketAddr) {
    let config = format!(
        "[machine.hub]\nenabled = true\nlisten = \"127.0.0.1:0\"\n[machine.hub.forward]\n\"/quay/\" = \"{server}\"\n"
    );
    manager
        .config_state
        .publish(Config::from_toml(&config).unwrap());
}

/// The response the hub's listener gives `request`, its head and body.
async fn respond(manager: &Arc<Manager>, request: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let manager = manager.clone();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let permit = Arc::new(Arc::new(Semaphore::new(1)).acquire_owned().await.unwrap());
        listener::serve(manager.hub.clone(), manager, stream, permit).await;
    });
    let mut client = TcpStream::connect(address).await.unwrap();
    client.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    let read = client.read_to_string(&mut response);
    tokio::time::timeout(HANG, read).await.unwrap().unwrap();
    response
}

/// A request for `target` that `member` signed with `key` now, with
/// `nonce`.
fn signed(member: &MemberId, key: &Identity, target: &str, nonce: &str) -> String {
    let time = UnixTime::now().seconds() as u64;
    let nonce = nonce.parse().unwrap();
    let signed = app_servers::signed(member, time, &nonce, "GET", target);
    let signature = key.sign(&signed);
    format!(
        "GET {target} HTTP/1.1\r\nhost: hub\r\nconnection: close\r\n{}: {member}\r\n{}: {time}\r\n{}: {nonce}\r\n{}: {signature}\r\n\r\n",
        app_servers::MEMBER,
        app_servers::TIME,
        app_servers::NONCE,
        app_servers::SIGNATURE,
    )
}

#[tokio::test]
async fn a_paired_device_s_signed_request_reaches_the_server_once() {
    let (address, mut heads) = server().await;
    let (_root, manager, member, device) = hub(address).await;
    let fresh = nonce().to_string();
    let request = signed(&member, &device, "/quay/board?view=all", &fresh);
    let response = respond(&manager, request.clone()).await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.ends_with("hi"), "{response}");
    let head = heads.recv().await.unwrap().to_lowercase();
    assert!(head.starts_with("get /quay/board?view=all "), "{head}");
    assert!(head.contains(&format!("shoal-member: {member}")), "{head}");
    assert!(!head.contains(app_servers::SIGNATURE), "{head}");
    // The same request again, as whatever saw it might send it, and
    // again once the hub's daemon restarted.
    let replayed = respond(&manager, request.clone()).await;
    assert!(replayed.starts_with("HTTP/1.1 401"), "{replayed}");
    let restarted = Manager::open(manager.paths.clone()).await.unwrap();
    forwarding(&restarted, address);
    let replayed = respond(&restarted, request).await;
    assert!(replayed.starts_with("HTTP/1.1 401"), "{replayed}");
    assert!(heads.try_recv().is_err());
}

#[tokio::test]
async fn a_request_no_paired_member_signed_never_reaches_the_server() {
    let (address, mut heads) = server().await;
    let (_root, manager, member, device) = hub(address).await;
    let stranger = Identity::new(&KeyPair::generate().unwrap()).unwrap();
    let unsigned = "GET /quay/ HTTP/1.1\r\nhost: hub\r\nconnection: close\r\n\r\n".to_owned();
    let unknown = signed(
        &MemberId::generate(),
        &stranger,
        "/quay/",
        &nonce().to_string(),
    );
    let forged = signed(&member, &stranger, "/quay/", &nonce().to_string());
    // Signed for another target than it asks for.
    let moved =
        signed(&member, &device, "/quay/a", &nonce().to_string()).replace("/quay/a", "/quay/b");
    // Another device's ID beside the signed one.
    let doubled = signed(&member, &device, "/quay/", &nonce().to_string()).replace(
        "\r\n\r\n",
        &format!(
            "\r\n{}: {}\r\n\r\n",
            app_servers::MEMBER,
            MemberId::generate()
        ),
    );
    for request in [unsigned, unknown, forged, moved, doubled] {
        let response = respond(&manager, request.clone()).await;
        assert!(
            response.starts_with("HTTP/1.1 401"),
            "{request}\n{response}"
        );
    }
    let id = member.clone();
    (manager.store)
        .write(move |tx| members::revoke(tx, &id))
        .await
        .unwrap();
    let revoked = signed(&member, &device, "/quay/", &nonce().to_string());
    let response = respond(&manager, revoked).await;
    assert!(response.starts_with("HTTP/1.1 401"), "{response}");
    let elsewhere = signed(&member, &device, "/other/", &nonce().to_string());
    let response = respond(&manager, elsewhere).await;
    assert!(response.starts_with("HTTP/1.1 404"), "{response}");
    assert!(heads.try_recv().is_err());
}

/// A server that upgrades each connection and then echoes what comes.
async fn echo_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_head(&mut stream).await;
        let answer =
            b"HTTP/1.1 101 Switching Protocols\r\nconnection: upgrade\r\nupgrade: echo\r\n\r\n";
        stream.write_all(answer).await.unwrap();
        let (mut reader, mut writer) = stream.split();
        let _ = tokio::io::copy(&mut reader, &mut writer).await;
    });
    address
}

/// Read up to the end of an HTTP head from `stream`.
async fn read_head(stream: &mut TcpStream) -> String {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).await.unwrap();
        head.push(byte[0]);
    }
    String::from_utf8(head).unwrap()
}

#[tokio::test]
async fn an_upgrade_carries_bytes_both_ways_and_holds_its_connection_s_permit() {
    let (_root, manager, member, device) = hub(echo_server().await).await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let connections = Arc::new(Semaphore::new(1));
    let permit = Arc::new(connections.clone().acquire_owned().await.unwrap());
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        listener::serve(manager.hub.clone(), manager, stream, permit).await;
    });
    let mut client = TcpStream::connect(address).await.unwrap();
    let request = signed(&member, &device, "/quay/live", &nonce().to_string())
        .replace("connection: close", "connection: upgrade\r\nupgrade: echo");
    client.write_all(request.as_bytes()).await.unwrap();
    let head = tokio::time::timeout(HANG, read_head(&mut client))
        .await
        .unwrap();
    assert!(head.starts_with("HTTP/1.1 101"), "{head}");
    client.write_all(b"ping").await.unwrap();
    let mut echoed = [0; 4];
    let read = client.read_exact(&mut echoed);
    tokio::time::timeout(HANG, read).await.unwrap().unwrap();
    assert_eq!(&echoed, b"ping");
    // The connection goes on, so it still counts against the listener's.
    assert_eq!(connections.available_permits(), 0);
    drop(client);
    let released = connections.acquire();
    let _released = tokio::time::timeout(HANG, released).await.unwrap().unwrap();
}
