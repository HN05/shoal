use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf};

use super::{exchange, send_package};
use shoal_protocol::protocol::{self, Body, ErrorCode, Response};

/// A package whose reading fails after `before` bytes.
struct Failing {
    before: Vec<u8>,
}

impl AsyncRead for Failing {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.before.is_empty() {
            return Poll::Ready(Err(io::Error::other("the pipe broke")));
        }
        let before = std::mem::take(&mut self.before);
        buf.put_slice(&before);
        Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn a_package_that_cannot_be_read_still_ends_for_the_daemon() {
    let (mut client, mut daemon) = tokio::net::UnixStream::pair().unwrap();
    let mut package = Failing {
        before: b"shoal workspace".to_vec(),
    };

    let read = send_package(&mut package, &mut client).await;

    assert!(format!("{:#}", read.unwrap_err()).contains("the pipe broke"));
    let mut received = Vec::new();
    daemon.read_to_end(&mut received).await.unwrap();
    assert_eq!(received, b"shoal workspace");
}

/// A package that sends its header and then nothing, never ending.
struct Stalled {
    header: Vec<u8>,
}

impl AsyncRead for Stalled {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.header.is_empty() {
            return Poll::Pending;
        }
        let header = std::mem::take(&mut self.header);
        buf.put_slice(&header);
        Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn a_refusal_ends_a_package_that_never_ends() {
    let (client, mut daemon) = tokio::net::UnixStream::pair().unwrap();
    let mut package = Stalled {
        header: b"shoal workspace 0.0.1\n".to_vec(),
    };
    let refusing = async move {
        let mut header = [0; 22];
        daemon.read_exact(&mut header).await.unwrap();
        let refusal = Body::error(ErrorCode::NotImported, "another version");
        protocol::write(&mut daemon, &Response::new(1, refusal))
            .await
            .unwrap();
        // It stays open, as a daemon answering other requests does.
        daemon
    };

    let exchanged = exchange(&mut package, tokio::io::BufReader::new(client));
    let ((read, answer), _daemon) = tokio::join!(exchanged, refusing);

    assert!(read.is_ok());
    let refused = <()>::try_from(answer.unwrap().body).unwrap_err();
    assert!(format!("{refused:#}").contains("another version"));
}
