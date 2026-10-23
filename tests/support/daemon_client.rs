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
