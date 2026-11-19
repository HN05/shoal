use std::path::PathBuf;

use shoal_protocol::{
    protocol::{MAX_FRAME, Request, WorkspaceMethod},
    space_names::SpaceSelector,
};

use super::{Chunk, Cursor};

/// A chunk from `offset` of a file then `size` long.
fn chunk(offset: u64, size: u64) -> Chunk {
    Chunk {
        offset,
        size,
        reset: false,
    }
}

/// A session file of the many a long session keeps, by its number.
fn file(number: usize) -> PathBuf {
    let tool_result = "toolu_".to_owned() + &"x".repeat(150);
    format!("-home-someone-project/session/tool-results/{tool_result}-{number}.txt").into()
}

/// A cursor that read `files` files, the last of them the furthest.
fn read(files: usize) -> Cursor {
    let mut cursor = Cursor::default();
    for number in 0..files {
        let read = number as u64 + 1;
        cursor.take(&file(number), chunk(0, read), vec![0; number + 1]);
    }
    cursor
}

#[test]
fn following_again_after_many_files_fits_in_a_frame_and_keeps_the_furthest_read() {
    let mut cursor = read(2_000);
    let from = cursor.resume();
    assert_eq!(from.get(&file(1_999)).map(|read| read.offset), Some(2_000));
    let method = WorkspaceMethod::FollowSession {
        space: SpaceSelector::id(&"space".into()),
        session: None,
        from,
    };
    let request = serde_json::to_vec(&Request::new(method)).unwrap();
    assert!(request.len() < MAX_FRAME, "{} bytes", request.len());
}

#[test]
fn what_the_daemon_sends_again_from_a_file_s_start_is_read_only_past_where_it_was() {
    let mut cursor = read(2_000);
    let from = cursor.resume();
    let left_out = file(0);
    assert!(!from.contains_key(&left_out));
    // The file left out comes again from its start, then grows.
    assert_eq!(cursor.take(&left_out, chunk(0, 3), vec![0; 1]), None);
    assert_eq!(
        cursor.take(&left_out, chunk(1, 3), vec![7; 2]),
        Some((1, vec![7; 2]))
    );
    // One that was read to 2 and comes again with a byte more.
    let grown = file(1);
    assert!(!from.contains_key(&grown));
    assert_eq!(
        cursor.take(&grown, chunk(0, 3), vec![9; 3]),
        Some((2, vec![9]))
    );
}

#[test]
fn a_file_left_out_that_its_tool_wrote_again_shorter_comes_whole() {
    let mut cursor = read(2_000);
    let from = cursor.resume();
    let rewritten = file(99);
    assert!(!from.contains_key(&rewritten));
    // Read to 100 before, it is 50 bytes long now.
    assert_eq!(
        cursor.take(&rewritten, chunk(0, 50), vec![5; 50]),
        Some((0, vec![5; 50]))
    );
    assert_eq!(
        cursor.take(&rewritten, chunk(50, 51), vec![6]),
        Some((50, vec![6]))
    );
}

#[test]
fn a_file_left_out_that_was_emptied_comes_again_as_an_empty_reset() {
    let mut cursor = read(2_000);
    let from = cursor.resume();
    let emptied = file(99);
    assert!(!from.contains_key(&emptied));
    assert_eq!(
        cursor.take(&emptied, chunk(0, 0), Vec::new()),
        Some((0, Vec::new()))
    );
    assert_eq!(
        cursor.take(&emptied, chunk(0, 1), vec![1]),
        Some((0, vec![1]))
    );
}

#[test]
fn a_file_the_daemon_resets_while_sending_it_again_comes_whole() {
    let mut cursor = read(2_000);
    let from = cursor.resume();
    let rewritten = file(99);
    assert!(!from.contains_key(&rewritten));
    // Read to 100 before, it came again longer, then shorter than it was
    // between two chunks, which the daemon sends again from 0.
    assert_eq!(cursor.take(&rewritten, chunk(0, 200), vec![1; 50]), None);
    let reset = Chunk {
        reset: true,
        ..chunk(0, 150)
    };
    assert_eq!(
        cursor.take(&rewritten, reset, vec![2; 150]),
        Some((0, vec![2; 150]))
    );
}

#[test]
fn a_file_read_in_part_then_written_anew_shorter_comes_whole_however_far_it_was_read() {
    let mut cursor = read(2_000);
    // Read to 10 of 200 bytes, then left out.
    let partial = file(4_000);
    cursor.take(&partial, chunk(0, 200), vec![0; 10]);
    let from = cursor.resume();
    assert!(!from.contains_key(&partial));
    // Written anew 150 bytes long, past where it was read to.
    assert_eq!(
        cursor.take(&partial, chunk(0, 150), vec![3; 150]),
        Some((0, vec![3; 150]))
    );
}

#[test]
fn a_file_sent_again_longer_then_shorter_across_two_returns_comes_whole() {
    let mut cursor = read(2_000);
    let rewritten = file(99);
    cursor.resume();
    // Read to 100 before, it comes again 200 bytes long, and the follow
    // returns again before it is past what was read.
    assert_eq!(cursor.take(&rewritten, chunk(0, 200), vec![1; 50]), None);
    let from = cursor.resume();
    assert!(!from.contains_key(&rewritten));
    // Now 150 bytes long: shorter than it was when last seen.
    assert_eq!(
        cursor.take(&rewritten, chunk(0, 150), vec![2; 150]),
        Some((0, vec![2; 150]))
    );
}
