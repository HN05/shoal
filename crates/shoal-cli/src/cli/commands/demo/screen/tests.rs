use super::*;

#[test]
fn a_screen_keeps_its_last_lines_without_trailing_blanks() {
    assert_eq!(
        last_lines("one\ntwo\nthree\n\n\n", 2).as_deref(),
        Some("two\nthree")
    );
    assert_eq!(last_lines("one\n", 5).as_deref(), Some("one"));
    assert_eq!(last_lines("\n\n", 5), None);
}
