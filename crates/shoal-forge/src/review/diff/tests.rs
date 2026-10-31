use super::*;

const DIFF: &str = "\
diff --git a/src/a.rs b/src/a.rs
index 1111111..2222222 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,4 +1,4 @@
 fn a() {
--- removed
+++ added
 }

@@ -10,2 +10,3 @@ fn b() {
 b();
+c();
 }
\\ No newline at end of file
diff --git a/old.rs b/new.rs
similarity index 90%
rename from old.rs
rename to new.rs
--- a/old.rs
+++ b/new.rs
@@ -3 +3 @@
-x
+y
diff --git a/gone.rs b/gone.rs
deleted file mode 100644
--- a/gone.rs
+++ /dev/null
@@ -1,2 +0,0 @@
-one
-two
diff --git \"a/sp\\303\\251cial.png\" \"b/sp\\303\\251cial.png\"
new file mode 100644
Binary files /dev/null and \"b/sp\\303\\251cial.png\" differ
diff --git a/same name.txt b/same name.txt
old mode 100644
new mode 100755
";

fn line(old: Option<u64>, new: Option<u64>) -> HunkLine {
    HunkLine { old, new }
}

fn comment(path: &str, number: u64, side: Side) -> LineComment {
    LineComment {
        path: path.into(),
        line: number,
        side,
        body: "Why?".into(),
    }
}

#[test]
fn a_git_diff_reads_each_file_with_its_paths_and_hunk_lines() -> Result<()> {
    let diff = Diff::parse(DIFF)?;
    let paths: Vec<_> = diff
        .files
        .iter()
        .map(|file| (file.old_path.as_str(), file.path.as_str()))
        .collect();
    assert_eq!(
        paths,
        [
            ("src/a.rs", "src/a.rs"),
            ("old.rs", "new.rs"),
            ("gone.rs", "gone.rs"),
            ("spécial.png", "spécial.png"),
            ("same name.txt", "same name.txt"),
        ]
    );
    assert_eq!(
        diff.files[0].lines,
        [
            line(Some(1), Some(1)),
            line(Some(2), None),
            line(None, Some(2)),
            line(Some(3), Some(3)),
            line(Some(4), Some(4)),
            line(Some(10), Some(10)),
            line(None, Some(11)),
            line(Some(11), Some(12)),
        ]
    );
    assert_eq!(
        diff.files[1].lines,
        [line(Some(3), None), line(None, Some(3))]
    );
    assert_eq!(
        diff.files[2].lines,
        [line(Some(1), None), line(Some(2), None)]
    );
    assert!(diff.files[3].lines.is_empty());
    Ok(())
}

#[test]
fn hunks_that_count_fewer_lines_than_they_show_are_refused() {
    let error = FileDiff::new("a".into(), "a".into(), "@@ -1 +1 @@\n-a\n-b\n+c").unwrap_err();
    assert!(format!("{error:#}").contains("more old lines"));
    assert!(Diff::parse("junk\ndiff --git a/a b/a\n").is_err());
}

#[test]
fn comments_are_placed_on_the_line_their_side_numbers() -> Result<()> {
    let diff = Diff::parse(DIFF)?;
    let comments = [
        comment("src/a.rs", 2, Side::New),
        comment("src/a.rs", 2, Side::Old),
        comment("src/a.rs", 12, Side::New),
        comment("old.rs", 3, Side::Old),
        comment("gone.rs", 2, Side::Old),
    ];
    let placed: Vec<_> = diff
        .place(&comments)?
        .into_iter()
        .map(|placed| (placed.file.path.as_str(), placed.line))
        .collect();
    assert_eq!(
        placed,
        [
            ("src/a.rs", line(None, Some(2))),
            ("src/a.rs", line(Some(2), None)),
            ("src/a.rs", line(Some(11), Some(12))),
            ("new.rs", line(Some(3), None)),
            ("gone.rs", line(Some(2), None)),
        ]
    );
    Ok(())
}

#[test]
fn comments_off_the_diff_refuse_the_review_naming_each() -> Result<()> {
    let diff = Diff::parse(DIFF)?;
    let comments = [
        comment("src/a.rs", 2, Side::New),
        comment("src/a.rs", 7, Side::New),
        comment("old.rs", 3, Side::New),
        comment("src/b.rs", 1, Side::Old),
    ];
    let error = diff.place(&comments).unwrap_err().to_string();
    assert_eq!(
        error,
        "these line comments are not on lines the PR's diff shows, so nothing was sent:\n  \
         src/a.rs:7: no hunk of the diff shows this line\n  \
         old.rs:3: the diff does not change old.rs\n  \
         src/b.rs:1 (old): the diff does not change src/b.rs"
    );
    Ok(())
}

#[test]
fn a_diff_shows_the_lines_of_its_hunks_in_the_new_version() -> Result<()> {
    let diff = Diff::parse(DIFF)?;
    assert!(diff.shows("src/a.rs", 11));
    assert!(diff.shows("new.rs", 3));
    assert!(!diff.shows("src/a.rs", 5));
    assert!(!diff.shows("src/b.rs", 1));
    Ok(())
}
