use tokio::io::BufReader;

use super::{Channel, QUEUED, Sender};

/// A channel whose other side never reads, with its queue filled behind a
/// write that cannot finish; the other side's end comes along to stay open.
fn stalled() -> (Channel, Sender, tokio::io::DuplexStream) {
    let (ours, theirs) = tokio::io::duplex(64);
    let (read, write) = tokio::io::split(ours);
    let channel = Channel::lines(BufReader::new(read), write);
    let sender = channel.sender();
    let frame = "x".repeat(4096);
    for _ in 0..=QUEUED {
        let _ = sender.try_send(&frame);
    }
    (channel, sender, theirs)
}

#[tokio::test(start_paused = true)]
async fn a_loop_never_waits_on_a_peer_that_stopped_reading() {
    let (_channel, sender, _theirs) = stalled();
    assert!(sender.try_send(&"ping").is_err());
}

#[tokio::test(start_paused = true)]
async fn a_written_send_gives_up_on_a_peer_that_stopped_reading() {
    let (_channel, sender, _theirs) = stalled();
    assert!(sender.send_written(&"reply").await.is_err());
}
