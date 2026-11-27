use shoal_protocol::hub::link::{Call, Reply};
use tokio::io::BufReader;

use super::{Calls, End, serve};
use crate::transport::Channel;

#[tokio::test(start_paused = true)]
async fn a_call_fails_once_its_session_ends_without_waiting_for_its_timeout() {
    let (ours, theirs) = tokio::io::duplex(1 << 16);
    let (read, write) = tokio::io::split(ours);
    let channel = Channel::lines(BufReader::new(read), write);
    let calls = Calls::new(channel.sender(), "the hub");
    let answer = |_: Call| async { (Reply::Left, ()) };
    let (end, call) = tokio::join!(
        async {
            let ended = serve(channel, &calls, answer, std::future::pending()).await;
            assert!(matches!(ended, End::Closed));
        },
        async {
            let call = calls.call(Call::Members);
            tokio::pin!(call);
            // The call is out; the other side then hangs up unanswered.
            tokio::select! {
                biased;
                result = &mut call => return result,
                () = tokio::task::yield_now() => {}
            }
            drop(theirs);
            call.await
        },
    );
    let () = end;
    let error = call.unwrap_err().to_string();
    assert!(error.contains("closed"), "{error}");
    assert!(calls.call(Call::Members).await.is_err());
}

#[tokio::test(start_paused = true)]
async fn a_call_its_caller_gives_up_on_waits_for_no_reply() {
    let (ours, _theirs) = tokio::io::duplex(1 << 16);
    let (read, write) = tokio::io::split(ours);
    let channel = Channel::lines(BufReader::new(read), write);
    let calls = Calls::new(channel.sender(), "the hub");
    let waited = std::time::Duration::from_secs(3);
    let given_up = tokio::time::timeout(waited, calls.call(Call::Members)).await;
    assert!(given_up.is_err());
    let pending = calls.pending.lock().unwrap();
    assert_eq!(pending.as_ref().map(|pending| pending.len()), Some(0));
}
