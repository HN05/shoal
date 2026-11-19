use std::fs;

use super::{Client, Stopped, read_from, send_file_gained};

#[test]
fn a_file_is_read_on_from_where_the_last_read_ended() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    fs::write(&path, "first\n").unwrap();
    assert_eq!(read_from(&path, 0).unwrap(), (0, b"first\n".to_vec()));
    fs::write(&path, "first\nsecond\n").unwrap();
    assert_eq!(read_from(&path, 6).unwrap(), (6, b"second\n".to_vec()));
    assert_eq!(read_from(&path, 13).unwrap(), (13, Vec::new()));
}

#[test]
fn a_file_that_shrank_is_read_again_from_its_start() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    fs::write(&path, "new\n").unwrap();
    assert_eq!(read_from(&path, 13).unwrap(), (0, b"new\n".to_vec()));
}

#[test]
fn a_large_file_comes_a_chunk_at_a_time() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    fs::write(&path, vec![b'x'; super::CHUNK + 1]).unwrap();
    let (_, first) = read_from(&path, 0).unwrap();
    assert_eq!(first.len(), super::CHUNK);
    let (offset, rest) = read_from(&path, first.len() as u64).unwrap();
    assert_eq!((offset, rest.len()), (super::CHUNK as u64, 1));
}

#[test]
fn a_whole_chunk_of_a_file_with_the_longest_path_fits_in_a_frame() {
    use shoal_protocol::{
        protocol::{Body, MAX_FRAME, Response},
        transcripts::SessionBytes,
    };
    let path = "p".repeat(4096);
    let bytes = SessionBytes::new(path.into(), u64::MAX, &[0xff; super::CHUNK]);
    let frame = Response::new(u64::MAX, Body::SessionBytes(bytes));
    assert!(serde_json::to_vec(&frame).unwrap().len() < MAX_FRAME);
}

/// A follow's client on one end of a new connection, and the other end.
fn client(manager: &crate::daemon::workspace::Manager) -> (Client<'_>, tokio::net::UnixStream) {
    let (ours, theirs) = tokio::net::UnixStream::pair().unwrap();
    let (_, writer) = ours.into_split();
    let client = Client {
        manager,
        writer,
        id: 1,
        scope: None,
    };
    (client, theirs)
}

#[tokio::test]
async fn a_follow_writes_nothing_once_a_restart_holds_operations_off() {
    let (_root, manager) = crate::daemon::test_support::manager().await;
    let (mut client, mut theirs) = client(&manager);
    let _quiet = manager.gates.quiesce().await;
    let sent = client.send(shoal_protocol::protocol::Body::Ok).await;
    let stopped = sent.unwrap_err().downcast::<Stopped>().unwrap();
    assert!(matches!(stopped, Stopped::Restarting), "{stopped}");
    drop(client);
    let mut written = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut theirs, &mut written)
        .await
        .unwrap();
    assert!(written.is_empty());
}

#[tokio::test]
async fn a_session_file_that_is_gone_waits_and_one_that_cannot_be_read_ends_the_follow() {
    let (root, manager) = crate::daemon::test_support::manager().await;
    let (mut client, _theirs) = client(&manager);
    let mut offsets = std::collections::HashMap::new();
    let gone = root.path().join("gone.jsonl");
    send_file_gained(&mut client, &gone, "gone.jsonl".into(), &mut offsets)
        .await
        .unwrap();
    // A directory opens but cannot be read.
    let unreadable = root.path().join("unreadable.jsonl");
    fs::create_dir(&unreadable).unwrap();
    let file = "unreadable.jsonl".into();
    let failed = send_file_gained(&mut client, &unreadable, file, &mut offsets).await;
    let stopped = failed.unwrap_err().downcast::<Stopped>().unwrap();
    assert!(matches!(stopped, Stopped::Unreadable(_)), "{stopped}");
}
