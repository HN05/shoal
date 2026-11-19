use std::fs;

use shoal_protocol::transcripts::ReadTo;

use super::{Client, Stopped, read_from, send_file_gained};

/// Where a read of `path` from `offset`, where it was read to then, starts
/// and the bytes it gains.
fn read(path: &std::path::Path, offset: u64) -> (u64, Vec<u8>) {
    let gained = read_from(
        path,
        ReadTo {
            offset,
            size: offset,
        },
    )
    .unwrap();
    (gained.offset, gained.bytes)
}

#[test]
fn a_file_is_read_on_from_where_the_last_read_ended() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    fs::write(&path, "first\n").unwrap();
    assert_eq!(read(&path, 0), (0, b"first\n".to_vec()));
    fs::write(&path, "first\nsecond\n").unwrap();
    assert_eq!(read(&path, 6), (6, b"second\n".to_vec()));
    assert_eq!(read(&path, 13), (13, Vec::new()));
}

#[test]
fn a_file_that_shrank_is_read_again_from_its_start() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    fs::write(&path, "new\n").unwrap();
    let seen = ReadTo {
        offset: 13,
        size: 13,
    };
    let gained = read_from(&path, seen).unwrap();
    assert_eq!((gained.offset, gained.size, gained.rewritten), (0, 4, true));
}

#[test]
fn a_file_shorter_than_it_was_though_not_than_its_offset_is_read_again_from_its_start() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    fs::write(&path, "rewritten\n").unwrap();
    // Read to 4 when it was 20 bytes long.
    let seen = ReadTo {
        offset: 4,
        size: 20,
    };
    let gained = read_from(&path, seen).unwrap();
    assert_eq!((gained.offset, gained.rewritten), (0, true));
    assert_eq!(gained.bytes, b"rewritten\n");
}

#[test]
fn a_large_file_comes_a_chunk_at_a_time() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    fs::write(&path, vec![b'x'; super::CHUNK + 1]).unwrap();
    let (_, first) = read(&path, 0);
    assert_eq!(first.len(), super::CHUNK);
    let (offset, rest) = read(&path, first.len() as u64);
    assert_eq!((offset, rest.len()), (super::CHUNK as u64, 1));
}

#[test]
fn a_whole_chunk_of_a_file_with_the_longest_path_fits_in_a_frame() {
    use shoal_protocol::{
        protocol::{Body, MAX_FRAME, Response},
        transcripts::SessionBytes,
    };
    let path = "p".repeat(4096);
    let bytes = SessionBytes::new(path.into(), u64::MAX, u64::MAX, &[0xff; super::CHUNK]);
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

/// The chunk a follow that read the empty file `emptied.jsonl` as `seen`
/// says, if at all, sends of it.
async fn chunk_of_empty_file(seen: Option<ReadTo>) -> serde_json::Value {
    let (root, manager) = crate::daemon::test_support::manager().await;
    let (mut client, theirs) = client(&manager);
    let emptied = root.path().join("emptied.jsonl");
    fs::write(&emptied, "").unwrap();
    let file: std::path::PathBuf = "emptied.jsonl".into();
    let mut read = std::collections::HashMap::new();
    if let Some(seen) = seen {
        read.insert(file.clone(), seen);
    }
    send_file_gained(&mut client, &emptied, file, &mut read)
        .await
        .unwrap();
    let mut line = String::new();
    let mut theirs = tokio::io::BufReader::new(theirs);
    tokio::io::AsyncBufReadExt::read_line(&mut theirs, &mut line)
        .await
        .unwrap();
    let frame: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(frame["type"], "session_bytes", "{frame}");
    let chunk = &frame["data"];
    assert_eq!(
        (&chunk["offset"], &chunk["size"], &chunk["data"]),
        (&0.into(), &0.into(), &"".into()),
        "{frame}"
    );
    chunk.clone()
}

#[tokio::test]
async fn a_file_emptied_since_it_was_read_comes_again_as_an_empty_reset() {
    let chunk = chunk_of_empty_file(Some(ReadTo { offset: 5, size: 5 })).await;
    assert_eq!(chunk["reset"], true, "{chunk}");
}

#[tokio::test]
async fn an_empty_file_a_follow_reads_first_comes_as_an_empty_chunk() {
    // As one emptied while the client was away, which it read before.
    let chunk = chunk_of_empty_file(None).await;
    assert!(chunk["reset"].is_null(), "{chunk}");
}
