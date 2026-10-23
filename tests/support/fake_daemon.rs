// Each integration test uses only the transport operations its fixture needs.
#![allow(dead_code)]

use serde_json::Value;
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::Path,
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

// A generous hang guard, never a timing assertion.
const HANG_GUARD: Duration = Duration::from_secs(60);

pub fn bind_fake_daemon(root: &Path) -> UnixListener {
    std::fs::create_dir_all(root.join("state")).unwrap();
    UnixListener::bind(root.join("state/daemon.sock")).unwrap()
}

/// Return None for the empty connection used to stop a fixture's server loop.
pub fn read_request(stream: &UnixStream) -> Option<Value> {
    stream.set_nonblocking(false).unwrap();
    stream.set_read_timeout(Some(HANG_GUARD)).unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).unwrap();
    if line.trim().is_empty() {
        return None;
    }
    Some(serde_json::from_str(&line).unwrap())
}

/// Keep envelope fields and newline delimiting identical for every response,
/// including fixtures that deliberately write several frames together.
pub fn frame(request: &Value, reply: Value) -> String {
    frame_with_protocol(request, request["protocol"].clone(), reply)
}

/// Answer under another protocol version, as a daemon from another release would.
pub fn frame_with_protocol(
    request: &Value,
    protocol: impl Into<Value>,
    mut reply: Value,
) -> String {
    reply["protocol"] = protocol.into();
    reply["id"] = request["id"].clone();
    format!("{reply}\n")
}

pub fn write_reply(mut stream: &UnixStream, request: &Value, reply: Value) {
    stream.set_write_timeout(Some(HANG_GUARD)).unwrap();
    stream.write_all(frame(request, reply).as_bytes()).unwrap();
}

/// Answer a single connection and return its request for test assertions.
pub fn answer(stream: UnixStream, f: impl FnOnce(&Value) -> Value) -> Value {
    let request = read_request(&stream).expect("missing fake daemon request");
    write_reply(&stream, &request, f(&request));
    request
}

pub async fn read_request_async(stream: &mut tokio::net::UnixStream) -> Value {
    let mut line = String::new();
    tokio::time::timeout(
        HANG_GUARD,
        tokio::io::BufReader::new(stream).read_line(&mut line),
    )
    .await
    .expect("missing fake daemon request")
    .unwrap();
    serde_json::from_str(&line).unwrap()
}

pub async fn write_reply_async(stream: &mut tokio::net::UnixStream, request: &Value, reply: Value) {
    tokio::time::timeout(
        HANG_GUARD,
        stream.write_all(frame(request, reply).as_bytes()),
    )
    .await
    .expect("fake daemon reply stalled")
    .unwrap();
}
