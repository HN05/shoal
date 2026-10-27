use super::*;

#[test]
fn hook_events_name_their_session() {
    let event =
        br#"{"session_id":"0b0e5f3c-7d2a-4c55-9f1e-1a2b3c4d5e6f","hook_event_name":"Stop"}"#;
    let session = |input: &[u8]| first_value(input).as_ref().and_then(session_of);
    assert_eq!(
        session(&event[..]).as_deref(),
        Some("0b0e5f3c-7d2a-4c55-9f1e-1a2b3c4d5e6f")
    );
    assert_eq!(session(br#"{"session_id":"--help"}"#), None);
    assert_eq!(session(br#"{"hook_event_name":"Stop"}"#), None);
    assert_eq!(session(b""), None);
}

#[test]
fn open_inputs_report_their_event_without_closing() -> std::io::Result<()> {
    use std::io::Write;

    let (reader, mut writer) = std::io::pipe()?;
    assert!(!readable_within(reader.as_fd(), Duration::from_millis(50)));
    writer.write_all(br#"{"session_id":"0b0e5f3c-7d2a-4c55-9f1e-1a2b3c4d5e6f"}"#)?;
    assert!(readable_within(reader.as_fd(), Duration::from_secs(10)));
    assert_eq!(
        first_value(&reader)
            .as_ref()
            .and_then(session_of)
            .as_deref(),
        Some("0b0e5f3c-7d2a-4c55-9f1e-1a2b3c4d5e6f")
    );
    drop(writer);
    assert!(readable_within(reader.as_fd(), Duration::from_secs(10)));
    Ok(())
}
