use std::sync::Arc;

use shoal_protocol::hub::link::{Call, Frame, Reply, STREAM_WINDOW};
use tokio::{io::BufReader, task::JoinHandle};

use super::{Incoming, Outgoing};
use crate::daemon::hub::{
    session::{Calls, serve},
    transport::Channel,
};

/// Two sides of a link, each serving it, whose calls each answer with
/// [`Reply::Left`].
struct Linked {
    opener: Arc<Calls>,
    writer: Arc<Calls>,
    _served: [JoinHandle<()>; 2],
}

fn linked() -> Linked {
    let (ours, theirs) = tokio::io::duplex(1 << 20);
    let side = |stream: tokio::io::DuplexStream, peer: &str| {
        let (read, write) = tokio::io::split(stream);
        let channel = Channel::lines(BufReader::new(read), write);
        let calls = Calls::new(channel.sender(), peer);
        let served = calls.clone();
        let task = tokio::spawn(async move {
            let answer = |_: Call| async { (Reply::Left, ()) };
            serve(channel, &served, answer, std::future::pending()).await;
        });
        (calls, task)
    };
    let (opener, first) = side(ours, "the writer");
    let (writer, second) = side(theirs, "the opener");
    Linked {
        opener,
        writer,
        _served: [first, second],
    }
}

impl Linked {
    /// A new stream the opener reads and the writer writes.
    fn stream(&self) -> (Incoming, Outgoing) {
        let incoming = self.opener.streams().open();
        let outgoing = self.writer.streams().writer(incoming.number());
        (incoming, outgoing)
    }
}

/// Everything `incoming` brings until it ends.
async fn read_all(incoming: &mut Incoming) -> Vec<u8> {
    let mut all = Vec::new();
    while let Some(bytes) = incoming.next().await.unwrap() {
        all.extend(bytes);
    }
    all
}

#[tokio::test]
async fn a_stalled_stream_holds_up_neither_calls_nor_other_streams() {
    let link = linked();
    let (mut stalled, mut stalled_writer) = link.stream();
    let (mut other, other_writer) = link.stream();
    let more_than_its_credit = vec![1; STREAM_WINDOW as usize * 2];
    let write = tokio::spawn(async move {
        stalled_writer.write(&more_than_its_credit).await.unwrap();
        stalled_writer.end(None).await;
    });
    let writing = tokio::spawn(async move {
        let mut other_writer = other_writer;
        other_writer.write(b"other bytes").await.unwrap();
        other_writer.end(None).await;
    });
    assert_eq!(read_all(&mut other).await, b"other bytes");
    writing.await.unwrap();
    assert!(matches!(
        link.opener.call(Call::Members).await.unwrap(),
        Reply::Left
    ));
    assert!(!write.is_finished());
    // Read, the stalled stream goes on to its end.
    assert_eq!(
        read_all(&mut stalled).await.len(),
        STREAM_WINDOW as usize * 2
    );
    write.await.unwrap();
}

#[tokio::test]
async fn a_reader_that_stops_stops_its_writer() {
    let link = linked();
    let (incoming, mut outgoing) = link.stream();
    drop(incoming);
    outgoing.closed().await;
    assert!(outgoing.write(b"unread").await.is_err());
}

#[tokio::test]
async fn a_writer_that_fails_ends_its_stream_with_the_error() {
    let link = linked();
    let (mut incoming, mut outgoing) = link.stream();
    outgoing.write(b"partial").await.unwrap();
    outgoing
        .end(Some("the session's machine left".into()))
        .await;
    assert_eq!(incoming.next().await.unwrap().unwrap(), b"partial");
    let error = incoming.next().await.unwrap_err().to_string();
    assert!(error.contains("machine left"), "{error}");
}

#[tokio::test]
async fn bytes_past_a_stream_s_credit_fail_the_link() {
    let link = linked();
    let (incoming, _outgoing) = link.stream();
    let past = Frame::Data {
        stream: incoming.number(),
        data: base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            vec![0; STREAM_WINDOW as usize + 1],
        ),
    };
    assert!(link.opener.streams().take(&past).is_err());
}

/// Fill `channel`'s send queue behind a frame its writer holds half
/// written, once the other side, `theirs`, read the frame's first byte,
/// which shows the writer took it.
async fn saturate(channel: &Channel, theirs: &mut tokio::io::DuplexStream) {
    channel.sender().send(&"x".repeat(4096)).await.unwrap();
    let mut first = [0; 1];
    tokio::io::AsyncReadExt::read_exact(theirs, &mut first)
        .await
        .unwrap();
    while channel.sender().try_send(&"x".repeat(4096)).is_ok() {}
}

/// The data frame of `bytes` on the stream `stream`.
fn data(stream: u64, bytes: &[u8]) -> Frame {
    Frame::Data {
        stream,
        data: base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, bytes),
    }
}

#[tokio::test]
async fn a_read_cancelled_while_the_link_is_saturated_loses_nothing() {
    let (ours, mut theirs) = tokio::io::duplex(64);
    let (read, write) = tokio::io::split(ours);
    let channel = Channel::lines(BufReader::new(read), write);
    let streams = super::Streams::new(channel.sender());
    let mut incoming = streams.open();
    let first = vec![1; STREAM_WINDOW as usize / 2];
    streams.take(&data(incoming.number(), &first)).unwrap();
    assert_eq!(incoming.next().await.unwrap().unwrap(), first);
    let second = vec![2; 10];
    streams.take(&data(incoming.number(), &second)).unwrap();
    // The read owes credit it cannot send yet; cancelled, it keeps both.
    saturate(&channel, &mut theirs).await;
    tokio::select! {
        biased;
        read = incoming.next() => panic!("read past a full queue: {read:?}"),
        () = tokio::task::yield_now() => {}
    }
    tokio::spawn(async move { tokio::io::copy(&mut theirs, &mut tokio::io::sink()).await });
    assert_eq!(incoming.next().await.unwrap().unwrap(), second);
}

#[tokio::test]
async fn a_reader_dropped_while_the_link_is_saturated_still_closes_its_stream() {
    let (ours, mut theirs) = tokio::io::duplex(64);
    let (read, write) = tokio::io::split(ours);
    let channel = Channel::lines(BufReader::new(read), write);
    let streams = super::Streams::new(channel.sender());
    let incoming = streams.open();
    let stream = incoming.number();
    saturate(&channel, &mut theirs).await;
    drop(incoming);
    let mut lines = tokio::io::AsyncBufReadExt::lines(BufReader::new(theirs));
    let close = serde_json::to_string(&Frame::Close { stream }).unwrap();
    while let Some(line) = lines.next_line().await.unwrap() {
        if line == close {
            return;
        }
    }
    panic!("the link ended without closing stream {stream}");
}
