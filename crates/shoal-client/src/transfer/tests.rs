use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf};

use super::send_package;

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
