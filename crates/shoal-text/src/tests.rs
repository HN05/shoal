use super::*;

#[test]
fn cut_bytes_keeps_text_within_the_limit() {
    assert_eq!(cut_bytes("short".to_owned(), 5), "short");
}

#[test]
fn cut_bytes_ends_at_a_character_boundary_with_the_mark() {
    let cut = cut_bytes("aé€😀 tail".to_owned(), 9);
    assert_eq!(cut, "aé€…");
    assert!(cut.len() <= 9);
}

#[test]
fn cut_bytes_trims_whitespace_before_the_mark() {
    assert_eq!(cut_bytes("one two three".to_owned(), 7), "one…");
}

#[test]
fn cut_bytes_leaves_out_a_mark_that_does_not_fit() {
    assert_eq!(cut_bytes("abcdef".to_owned(), 2), "ab");
}

#[test]
fn prefix_bytes_cuts_without_a_mark() {
    assert_eq!(prefix_bytes("aé€", 4), "aé");
}

#[test]
fn cut_middle_keeps_both_ends() {
    let line = format!("start {} end", "x".repeat(100));
    let cut = cut_middle(&line, 20);
    assert_eq!(cut, "start xx…xxxx end");
    assert!(cut.len() <= 20);
}

#[test]
fn width_counts_wide_characters_twice() {
    assert_eq!(width("a漢😀\u{301}"), 5);
}

#[test]
fn width_measures_an_emoji_sequence_as_one() {
    assert_eq!(width("👩\u{200D}💻"), 2);
    assert_eq!(width("❤\u{FE0F}"), 2);
}

#[test]
fn cut_width_keeps_an_emoji_sequence_whole() {
    assert_eq!(cut_width("ab👩\u{200D}💻cd".to_owned(), 3), "ab…");
}

#[test]
fn cut_fits_both_limits_at_once() {
    let text = format!("{}a{}", "a".repeat(39), "\u{301}".repeat(61));
    let cut = cut(text, 40, 160);
    assert_eq!(cut, format!("{}…", "a".repeat(39)));
}

#[test]
fn cut_width_keeps_wide_characters_within_the_columns() {
    let cut = cut_width("漢字のテキスト".to_owned(), 6);
    assert_eq!(cut, "漢字…");
    assert_eq!(width(&cut), 5);
}

#[test]
fn pad_fills_the_columns_a_wide_text_leaves() {
    assert_eq!(pad("漢a", 5), "漢a  ");
    assert_eq!(pad("漢字", 3), "漢字");
}

#[test]
fn cut_width_keeps_text_that_fits() {
    assert_eq!(cut_width("漢字".to_owned(), 4), "漢字");
}
