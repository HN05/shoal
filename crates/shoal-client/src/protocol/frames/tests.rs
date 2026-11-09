use super::*;
use crate::protocol::{Body, ExecutionEvent};

#[test]
fn frame_pages_split_long_listings_in_order() {
    assert_eq!(
        frame_pages(Vec::<String>::new()).unwrap(),
        [Vec::<String>::new()]
    );
    let items: Vec<String> = (0..300).map(|n| format!("{n:0>500}")).collect();
    let pages = frame_pages(items.clone()).unwrap();
    assert!(pages.len() > 1);
    for page in &pages {
        assert!(serde_json::to_vec(page).unwrap().len() < MAX_FRAME - 512);
    }
    assert_eq!(pages.concat(), items);
}

#[test]
fn text_pieces_fit_frames_and_keep_characters_whole() {
    assert!(text_pieces("").is_empty());
    // Quotes double when escaped; multibyte characters straddle piece ends.
    let text = format!("{}{}", "\"".repeat(MAX_FRAME), "é".repeat(MAX_FRAME));
    let pieces = text_pieces(&text);
    for page in frame_pages(pieces.clone()).unwrap() {
        assert!(serde_json::to_vec(&page).unwrap().len() < MAX_FRAME - 512);
    }
    assert_eq!(pieces.concat(), text);
}

#[tokio::test]
async fn buffered_execution_reads_preserve_adjacent_frames() {
    let (mut writer, reader) = tokio::io::duplex(1024);
    write(&mut writer, &ExecutionEvent::Finished { exit_code: 7 })
        .await
        .unwrap();
    write(&mut writer, &ExecutionEvent::Finished { exit_code: 8 })
        .await
        .unwrap();
    let mut reader = BufReader::new(reader);
    for expected in [7, 8] {
        let event: ExecutionEvent = read_buffered(&mut reader).await.unwrap();
        assert!(matches!(event, ExecutionEvent::Finished { exit_code } if exit_code == expected));
    }
}

#[tokio::test]
async fn a_frame_cut_off_by_a_closed_connection_reads_as_end_of_stream() {
    for sent in [&b""[..], b"{\"type\":\"fin"] {
        let (mut writer, reader) = tokio::io::duplex(1024);
        writer.write_all(sent).await.unwrap();
        drop(writer);
        let error = read::<ExecutionEvent>(&mut BufReader::new(reader))
            .await
            .unwrap_err();
        let error = error.downcast_ref::<std::io::Error>().unwrap();
        assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
    }
}

#[tokio::test(start_paused = true)]
async fn a_response_read_cancelled_partway_keeps_what_it_read() {
    use std::time::Duration;
    use tokio::io::AsyncWriteExt as _;
    let (mut writer, reader) = tokio::io::duplex(4 * MAX_FRAME);
    let mut responses = Responses::new(reader);
    let entries: Vec<String> = (0..4000).map(|n| format!("{n:>40}")).collect();
    let mut wire = Vec::new();
    for frame in Response::new(7, Body::ApiResponse(entries.clone()))
        .frames()
        .unwrap()
    {
        write(&mut wire, &frame).await.unwrap();
    }
    // Cancel once partway through the first page, and once after it, partway
    // through the next.
    let first_page = wire.iter().position(|byte| *byte == b'\n').unwrap() + 1;
    for end in [first_page / 2, first_page] {
        writer.write_all(&wire[..end]).await.unwrap();
        wire.drain(..end);
        let cancelled = tokio::time::timeout(Duration::from_secs(1), responses.next()).await;
        assert!(cancelled.is_err(), "the response is unfinished");
    }
    writer.write_all(&wire).await.unwrap();
    let response = responses.next().await.unwrap();
    assert_eq!(Vec::<String>::try_from(response.body).unwrap(), entries);
}
