use super::*;

#[test]
fn open_inputs_report_their_event_without_closing() -> std::io::Result<()> {
    use std::io::Write;

    let (reader, mut writer) = std::io::pipe()?;
    assert!(!readable_within(reader.as_fd(), Duration::from_millis(50)));
    writer.write_all(br#"{"session_id":"0b0e5f3c-7d2a-4c55-9f1e-1a2b3c4d5e6f"}"#)?;
    assert!(readable_within(reader.as_fd(), Duration::from_secs(10)));
    assert_eq!(
        first_value(&reader).map(|event| event["session_id"].clone()),
        Some("0b0e5f3c-7d2a-4c55-9f1e-1a2b3c4d5e6f".into())
    );
    drop(writer);
    assert!(readable_within(reader.as_fd(), Duration::from_secs(10)));
    Ok(())
}

#[test]
fn empty_inputs_report_no_event() {
    assert_eq!(first_value(&b""[..]), None);
}
