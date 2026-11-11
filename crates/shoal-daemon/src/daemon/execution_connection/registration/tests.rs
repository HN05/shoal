use super::*;
use tokio::{io::BufReader, sync::oneshot};

async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(std::time::Duration::from_secs(60), future)
        .await
        .expect("registration stalled")
}

#[tokio::test(start_paused = true)]
async fn a_slow_registration_keeps_its_wrapper_waiting_until_it_leaves() {
    let (client, mut server) = UnixStream::pair().unwrap();
    let (finish, finished) = oneshot::channel::<()>();
    let registering = tokio::spawn(async move { keeping_alive(&mut server, 7, finished).await.1 });
    let mut client = BufReader::new(client);
    for _ in 0..3 {
        let response: Response = bounded(protocol::read_buffered(&mut client)).await.unwrap();
        assert_eq!(response.id, 7);
        assert!(
            matches!(response.body, Body::Keepalive),
            "{:?}",
            response.body
        );
    }
    drop(client);
    tokio::time::sleep(timing::REGISTRATION_KEEPALIVE * 2).await;
    finish.send(()).unwrap();
    let listening = bounded(registering).await.unwrap();
    assert!(!listening);
}
