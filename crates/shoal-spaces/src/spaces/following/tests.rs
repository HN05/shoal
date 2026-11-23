use std::fs;

use shoal_protocol::transcripts::ReadTo;

use crate::spaces::following::read_from;

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
    fs::write(&path, vec![b'x'; crate::spaces::following::CHUNK + 1]).unwrap();
    let (_, first) = read(&path, 0);
    assert_eq!(first.len(), crate::spaces::following::CHUNK);
    let (offset, rest) = read(&path, first.len() as u64);
    assert_eq!(
        (offset, rest.len()),
        (crate::spaces::following::CHUNK as u64, 1)
    );
}

#[test]
fn a_whole_chunk_of_a_file_with_the_longest_path_fits_in_a_frame() {
    use shoal_protocol::{
        protocol::{Body, MAX_FRAME, Response},
        transcripts::SessionBytes,
    };
    let path = "p".repeat(4096);
    let bytes = SessionBytes::new(
        path.into(),
        u64::MAX,
        u64::MAX,
        &[0xff; crate::spaces::following::CHUNK],
    );
    let frame = Response::new(u64::MAX, Body::SessionBytes(bytes));
    assert!(serde_json::to_vec(&frame).unwrap().len() < MAX_FRAME);
}
