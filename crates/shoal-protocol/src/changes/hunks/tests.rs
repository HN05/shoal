use super::*;

#[test]
fn a_hunk_splits_into_its_runs_of_changed_lines() {
    // As an edit tool reports it: from line 10 on, with context around.
    let lines = [" a", "-b", "+B", "+B2", " c", " d", "-e", " f"];
    let hunks = split(9, 9, lines);
    let places: Vec<(u32, Lines, usize, usize)> = (hunks.iter())
        .map(|hunk| {
            (
                hunk.before,
                hunk.lines,
                hunk.removed.len(),
                hunk.added.len(),
            )
        })
        .collect();
    assert_eq!(
        places,
        [
            (10, Lines { start: 10, end: 12 }, 1, 2),
            // Removed lines leave the point where they were.
            (13, Lines { start: 14, end: 14 }, 1, 0),
        ]
    );
    assert_eq!(hunks[0].added, ["B", "B2"]);
}

#[test]
fn a_patch_counts_its_sides_as_a_unified_diff_header_does() {
    let changed = &split(9, 9, ["-b", "+B", "+B2"])[0];
    assert_eq!(changed.patch(), "@@ -10 +10,2 @@\n-b\n+B\n+B2\n");
    let inserted = &split(header_start(4, 0), 4, ["+x"])[0];
    assert_eq!(inserted.patch(), "@@ -4,0 +5 @@\n+x\n");
    assert_eq!(Hunk::created("a\nb\n").lines, Lines { start: 0, end: 2 });
}

#[test]
fn a_diff_of_two_versions_gives_the_hunks_of_each_change() {
    let before = "a\nb\nc\nd\ne\n";
    let after = "a\nB\nc\nd\nnew\ne\n";
    let hunks = diff(before, after);
    let patches: Vec<String> = hunks.iter().map(Hunk::patch).collect();
    assert_eq!(patches, ["@@ -2 +2 @@\n-b\n+B\n", "@@ -4,0 +5 @@\n+new\n"]);
}

#[test]
fn undoing_a_change_s_hunks_gives_the_text_before_it() {
    let before = "a\nb\nc\nd\ne\n";
    let after = "a\nB\nc\nd\nnew\ne\n";
    assert_eq!(
        unapply(after, &diff(before, after)).as_deref(),
        Some(before)
    );
    // Someone else changed a line the hunk added.
    assert_eq!(unapply("a\nX\nc\nd\nnew\ne\n", &diff(before, after)), None);
}

#[test]
fn undoing_a_change_keeps_each_line_s_own_ending() {
    let before = "a\r\nb\r\nc";
    let after = "a\r\nB\r\nc";
    assert_eq!(
        unapply(after, &diff(before, after)).as_deref(),
        Some(before)
    );
}
