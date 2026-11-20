use super::*;

/// A file of the lines `a` to `j`, one per line.
const FILE: &str = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";

fn lines(text: &str) -> Lines {
    text.parse().unwrap()
}

fn followed(after: &str, range: &str) -> (String, CodeState) {
    let followed = follow(FILE, after, lines(range));
    (followed.lines.to_string(), followed.state)
}

#[test]
fn lines_read_and_show_from_one() {
    assert_eq!(lines("4-6"), Lines { start: 3, end: 6 });
    assert_eq!(lines("4"), Lines { start: 3, end: 4 });
    assert_eq!(lines("4-6").to_string(), "4-6");
    assert_eq!(lines("4").to_string(), "4");
    for invalid in ["0-2", "5-4", "x", "3-", ""] {
        assert!(invalid.parse::<Lines>().is_err(), "{invalid}");
    }
}

#[test]
fn a_range_shifts_by_lines_added_or_removed_above_it() {
    let added = "a\nnew\nnew\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
    assert_eq!(followed(added, "4-6"), ("6-8".into(), CodeState::Unchanged));
    let removed = "a\nd\ne\nf\ng\nh\ni\nj\n";
    assert_eq!(
        followed(removed, "4-6"),
        ("2-4".into(), CodeState::Unchanged)
    );
}

#[test]
fn lines_added_right_above_or_below_a_range_stay_outside_it() {
    let around = "a\nb\nc\nabove\nd\ne\nf\nbelow\ng\nh\ni\nj\n";
    assert_eq!(
        followed(around, "4-6"),
        ("5-7".into(), CodeState::Unchanged)
    );
}

#[test]
fn a_range_grows_or_shrinks_to_cover_edits_inside_it() {
    let grown = "a\nb\nc\nd\nnew\nnew\ne\nf\ng\nh\ni\nj\n";
    assert_eq!(followed(grown, "4-6"), ("4-8".into(), CodeState::Changed));
    let shrunk = "a\nb\nc\nd\nf\ng\nh\ni\nj\n";
    assert_eq!(followed(shrunk, "4-6"), ("4-5".into(), CodeState::Changed));
    let edited = "a\nb\nc\nd\nE\nf\ng\nh\ni\nj\n";
    assert_eq!(followed(edited, "4-6"), ("4-6".into(), CodeState::Changed));
}

#[test]
fn an_edit_across_a_boundary_extends_the_range_to_cover_it() {
    let across = "a\nb\nX\nY\ne\nf\ng\nh\ni\nj\n";
    assert_eq!(followed(across, "4-6"), ("3-6".into(), CodeState::Changed));
}

#[test]
fn a_deleted_range_keeps_the_point_where_it_was_and_stays_there() {
    let deleted = "a\nb\nc\ng\nh\ni\nj\n";
    let gone = follow(FILE, deleted, lines("4-6"));
    assert_eq!(gone.state, CodeState::Outdated);
    assert_eq!(gone.lines, Lines { start: 3, end: 3 });
    let later = follow(deleted, &format!("new\n{deleted}"), gone.lines);
    assert_eq!(later.lines, Lines { start: 4, end: 4 });
    assert_eq!(later.state, CodeState::Outdated);
}

#[test]
fn an_unchanged_file_keeps_its_range() {
    assert_eq!(followed(FILE, "4-6"), ("4-6".into(), CodeState::Unchanged));
}
