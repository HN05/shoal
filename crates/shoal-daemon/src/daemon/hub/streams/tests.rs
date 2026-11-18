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
