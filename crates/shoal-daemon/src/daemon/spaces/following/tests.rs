use std::fs;

use super::read_from;

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
