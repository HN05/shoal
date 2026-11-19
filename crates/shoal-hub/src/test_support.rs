//! What the daemon's tests of the hub share: two sides of a link.
use std::sync::Arc;

use shoal_protocol::hub::link::{Call, Reply};
use tokio::{io::BufReader, task::JoinHandle};

use crate::{
    session::{Calls, serve},
    streams::{Incoming, Outgoing},
    transport::Channel,
};

/// Two sides of a link, each serving it, whose calls each answer with
/// [`Reply::Left`].
pub struct Linked {
    pub opener: Arc<Calls>,
    pub writer: Arc<Calls>,
    _served: [JoinHandle<()>; 2],
}

pub fn linked() -> Linked {
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
    pub fn stream(&self) -> (Incoming, Outgoing) {
        let incoming = self.opener.streams().open();
        let (outgoing, _) = self.writer.streams().answering(incoming.number(), false);
        (incoming, outgoing)
    }
}
