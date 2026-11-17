use super::{SHOWN_TEXT, shown};

#[test]
fn what_the_other_side_sent_shows_as_one_short_printable_line() {
    assert_eq!(shown("revoked\n\u{1b}[2J"), "revoked  [2J");
    assert!(shown(&"x".repeat(1 << 20)).len() <= SHOWN_TEXT);
}
