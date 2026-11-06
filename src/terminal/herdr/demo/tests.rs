use super::*;

#[test]
fn pane_reads_take_the_text_from_json_or_plain_output() {
    let json = br#"{"id":"cli:pane:read","result":{"read":{"pane_id":"w1:p2","text":"line\n"},"type":"pane_read"}}"#;
    assert_eq!(read_text(json), "line\n");
    assert_eq!(read_text(b"plain\n"), "plain\n");
}
