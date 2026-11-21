#[test]
fn multi_line_alerts_stay_one_entry() {
    assert_eq!(
        super::hanging_indent("Move commits:\n\nabc123 to api\ndef456 to cli", "  "),
        "Move commits:\n\n  abc123 to api\n  def456 to cli"
    );
}
