//! The client side of the daemon's newline-delimited JSON protocol, for requests
//! the CLI cannot express. Fixtures build their own envelopes.
// Each integration test uses only the exchanges its fixture needs.
#![allow(dead_code)]

use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::Path,
};

/// Connect to the daemon of the test's state directory.
pub fn connect(root: &Path) -> UnixStream {
    let stream = UnixStream::connect(root.join("state/daemon.sock")).unwrap();
    stream.set_read_timeout(Some(super::HANG_GUARD)).unwrap();
    stream
}

pub fn send(mut stream: &UnixStream, request: &Value) {
    writeln!(stream, "{request}").unwrap();
}

/// Read and decode one response frame, naming what arrived when it is not one.
pub fn receive(stream: &UnixStream) -> Value {
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    assert!(
        !line.is_empty(),
        "daemon closed the connection without a response"
    );
    serde_json::from_str(&line).unwrap_or_else(|error| panic!("daemon response {line:?}: {error}"))
}

/// Send `request` on its own connection and return the response.
pub fn exchange(root: &Path, request: &Value) -> Value {
    let stream = connect(root);
    send(&stream, request);
    receive(&stream)
}

/// The protocol version the daemon answers a status request with.
pub fn protocol(root: &Path) -> u64 {
    exchange(root, &json!({"protocol": 0, "id": 1, "method": "status"}))["protocol"]
        .as_u64()
        .unwrap()
}

/// The ID of the space named `name`, as tests name the spaces they make: a
/// workspace, a chat, else a removed workspace. A name never selects a
/// space, so tests select one by what this returns; a name no space has
/// gives itself, which selects nothing.
pub fn space_id(root: &Path, name: &str) -> String {
    let protocol = protocol(root);
    let listed = |method: &str| {
        let response = exchange(
            root,
            &json!({"protocol": protocol, "id": 1, "method": method}),
        );
        response["data"].as_array().cloned().unwrap_or_default()
    };
    let named = |space: &&Value| space["name"] == name;
    let found = (listed("list_workspaces").iter().find(named))
        .or(listed("list_chats").iter().find(named))
        .map(|space| space["id"].clone())
        .or_else(|| {
            (listed("removed_workspaces").iter().find(named))
                .map(|removed| removed["workspace_id"].clone())
        });
    found
        .and_then(|id| id.as_str().map(str::to_owned))
        .unwrap_or_else(|| name.to_owned())
}
