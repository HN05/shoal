use super::*;

/// Every frame of `frames`, encoded one after another, read back in turn.
async fn read_back<T>(
    frames: &[T],
    encode: impl Fn(&T) -> Vec<u8>,
    read: impl AsyncFn(&mut &[u8]) -> Result<Option<T>>,
) -> Vec<T> {
    let bytes: Vec<u8> = frames.iter().flat_map(encode).collect();
    let mut reader = bytes.as_slice();
    let mut read_frames = Vec::new();
    while let Some(frame) = read(&mut reader).await.unwrap() {
        read_frames.push(frame);
    }
    read_frames
}

#[tokio::test]
async fn frames_from_the_viewer_read_back_as_written() {
    let frames = [
        ToSession::Keys(b"ls\r".to_vec()),
        ToSession::Resize(TerminalSize {
            rows: 50,
            columns: 300,
        }),
        ToSession::Keys(Vec::new()),
    ];
    let read = read_back(&frames, ToSession::encode, async |r| {
        ToSession::read(r).await
    })
    .await;
    assert_eq!(read, frames);
}

#[tokio::test]
async fn frames_from_the_session_read_back_as_written() {
    let frames = [
        FromSession::Session(ChosenSession {
            name: "shoal-7".into(),
            place: Origin::created(HostName::Zmx, "shoal-7".into()),
        }),
        FromSession::Screen(vec![0x1b, b'[', b'H']),
        FromSession::Left(true),
        FromSession::Left(false),
        FromSession::Exited(-2),
    ];
    let read = read_back(&frames, FromSession::encode, async |r| {
        FromSession::read(r).await
    })
    .await;
    assert_eq!(read, frames);
}

#[tokio::test]
async fn a_frame_cut_short_is_an_error_and_one_too_long_is_refused() {
    let mut cut = &FromSession::Screen(b"abc".to_vec()).encode()[..6];
    assert!(FromSession::read(&mut cut).await.is_err());
    let mut long = vec![SCREEN];
    long.extend_from_slice(&(MAX_FRAME as u32 + 1).to_be_bytes());
    assert!(FromSession::read(&mut long.as_slice()).await.is_err());
}

#[test]
fn an_attach_reports_the_session_it_chose_and_whether_it_left_it_open() {
    let chosen = AttachReport::Chosen(ChosenSession {
        name: "shoal-7".into(),
        place: Origin::created(HostName::Zmx, "shoal-7".into()),
    });
    let left = AttachReport::Left { open: true };
    for report in [chosen, left] {
        let line = serde_json::to_string(&report).unwrap();
        assert_eq!(serde_json::from_str::<AttachReport>(&line).unwrap(), report);
    }
    let left = serde_json::to_value(AttachReport::Left { open: false }).unwrap();
    assert_eq!(left, serde_json::json!({"left": {"open": false}}));
}

#[test]
fn a_viewer_s_terminal_is_written_beside_its_session() {
    let viewer = Viewer {
        session: Some("shoal-7".into()),
        again: None,
        terminal: Terminal {
            size: Some(TerminalSize {
                rows: 24,
                columns: 80,
            }),
            term: Some("xterm-ghostty".into()),
            apps: Vec::new(),
        },
    };
    let json = serde_json::to_value(&viewer).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "session": "shoal-7",
            "size": {"rows": 24, "columns": 80},
            "term": "xterm-ghostty",
        })
    );
    assert_eq!(serde_json::from_value::<Viewer>(json).unwrap(), viewer);
}
