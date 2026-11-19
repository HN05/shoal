use std::{net::SocketAddr, sync::Arc};

use shoal_ids::member_ids::MemberId;
use shoal_protocol::hub::{MemberKind, app_servers};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::Semaphore,
};

use crate::{
    config::Config,
    daemon::{
        hub::{
            identity::{Identity, KeyPair, nonce},
            listener, members,
        },
        manager::Manager,
        test_daemon::HANG,
        test_support,
    },
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
    let config = format!(
        "[machine.hub]\nenabled = true\nlisten = \"127.0.0.1:0\"\n[machine.hub.forward]\n\"/quay/\" = \"{server}\"\n"
    );
    manager
        .config_state
        .publish(Config::from_toml(&config).unwrap());
    let device = Identity::new(&KeyPair::generate().unwrap()).unwrap();
    let key = device.public.clone();
    let member = (manager.store)
        .write(move |tx| members::join(tx, MemberKind::Device, "phone", &key))
        .await
        .unwrap();
    (root, manager, member, device)
}

/// The response the hub's listener gives `request`, its head and body.
async fn respond(manager: &Arc<Manager>, request: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let manager = manager.clone();
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let permit = Arc::new(Arc::new(Semaphore::new(1)).acquire_owned().await.unwrap());
        listener::serve(manager, stream, permit).await;
    });
    let mut client = TcpStream::connect(address).await.unwrap();
    client.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    let read = client.read_to_string(&mut response);
    tokio::time::timeout(HANG, read).await.unwrap().unwrap();
    response
}

/// A request for `target` that `member` signed with `key`, with `nonce`.
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
    // The same request again, as whatever saw it might send it.
    let replayed = respond(&manager, request).await;
    assert!(replayed.starts_with("HTTP/1.1 401"), "{replayed}");
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
    for request in [unsigned, unknown, forged, moved] {
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
