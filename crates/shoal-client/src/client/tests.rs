use std::time::Duration;

use tokio::{net::UnixListener, sync::oneshot};

use super::*;
use crate::{
    AnswerQuestion, ListRepositories,
    wire::{FrameReader, Response},
};

/// A daemon that welcomes each connection; it reads nothing on the first
/// until `resume` fires, and answers every request on the others.
fn fake_daemon(
    listener: UnixListener,
    resume: oneshot::Receiver<()>,
) -> tokio::task::JoinHandle<usize> {
    tokio::spawn(async move {
        let mut resume = Some(resume);
        let mut connections = 0;
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            connections += 1;
            let (reader, mut writer) = stream.into_split();
            let mut reader = FrameReader::new(reader);
            let _: Opening = reader.read().await.unwrap().unwrap();
            let welcome = Welcome {
                version: crate::VERSION.into(),
                supported_majors: vec![1],
                features: vec![format!("connection {connections}")],
            };
            wire::write(&mut writer, &HandshakeReply::Welcome(welcome))
                .await
                .unwrap();
            if let Some(resume) = resume.take() {
                let _ = resume.await;
                tokio::spawn(async move {
                    while reader
                        .read::<serde_json::Value>()
                        .await
                        .is_ok_and(|f| f.is_some())
                    {}
                });
                continue;
            }
            let request: Request = reader.read().await.unwrap().unwrap();
            let reply = Reply::Result(serde_json::json!([]));
            wire::write(
                &mut writer,
                &Response {
                    id: request.id,
                    reply,
                },
            )
            .await
            .unwrap();
            return connections;
        }
    })
}

#[tokio::test]
async fn a_call_cancelled_mid_write_reconnects_for_the_next_with_its_welcome() {
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("daemon.sock");
    let (resume, resumed) = oneshot::channel();
    let daemon = fake_daemon(UnixListener::bind(&socket).unwrap(), resumed);
    let client = Client::connect(&socket).await.unwrap();
    assert!(client.welcome().offers("connection 1"));
    // Larger than any socket buffer, so its write cannot finish while the
    // daemon reads nothing.
    let large = AnswerQuestion {
        id: 1,
        answer: "x".repeat(MAX_REQUEST),
        shown: None,
    };
    let cancelled = tokio::time::timeout(Duration::from_millis(100), client.call(large)).await;
    assert!(cancelled.is_err(), "the write cannot finish: {cancelled:?}");
    resume.send(()).unwrap();
    let listed = client.call(ListRepositories {}).await.unwrap();
    assert!(listed.is_empty());
    assert_eq!(
        daemon.await.unwrap(),
        2,
        "the next call opens a new connection"
    );
    assert!(
        client.welcome().offers("connection 2"),
        "{:?}",
        client.welcome()
    );
}

/// A request nearly as large as a frame allows.
const MAX_REQUEST: usize = crate::wire::MAX_FRAME - 1024;
