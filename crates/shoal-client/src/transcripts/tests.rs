use std::path::PathBuf;

use shoal_protocol::{
    protocol::{MAX_FRAME, Request, WorkspaceMethod},
    space_names::SpaceSelector,
};

use super::Cursor;

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
        cursor.take(&file(number), 0, read, vec![0; number + 1]);
    }
    cursor
}

#[test]
fn following_again_after_many_files_fits_in_a_frame_and_keeps_the_furthest_read() {
    let mut cursor = read(2_000);
    let from = cursor.resume();
    assert_eq!(from.get(&file(1_999)), Some(&2_000));
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
    assert_eq!(cursor.take(&left_out, 0, 3, vec![0; 1]), None);
    assert_eq!(
        cursor.take(&left_out, 1, 3, vec![7; 2]),
        Some((1, vec![7; 2]))
    );
    // One that was read to 2 and comes again with a byte more.
    let grown = file(1);
    assert!(!from.contains_key(&grown));
    assert_eq!(cursor.take(&grown, 0, 3, vec![9; 3]), Some((2, vec![9])));
}

#[test]
fn a_file_left_out_that_its_tool_wrote_again_shorter_comes_whole() {
    let mut cursor = read(2_000);
    let from = cursor.resume();
    let rewritten = file(99);
    assert!(!from.contains_key(&rewritten));
    // Read to 100 before, it is 50 bytes long now.
    assert_eq!(
        cursor.take(&rewritten, 0, 50, vec![5; 50]),
        Some((0, vec![5; 50]))
    );
    assert_eq!(
        cursor.take(&rewritten, 50, 51, vec![6]),
        Some((50, vec![6]))
    );
}
