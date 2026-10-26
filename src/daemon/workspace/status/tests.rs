use super::*;

#[test]
fn numstat_counts_text_and_binary_files() {
    let summary = parse_numstat("2\t1\tfirst\n10\t0\tsecond\n-\t-\timage.png\n").unwrap();
    assert_eq!(summary.files_changed, 3);
    assert_eq!(summary.insertions, 12);
    assert_eq!(summary.deletions, 1);
}

#[test]
fn numstat_rejects_incomplete_records() {
    assert!(parse_numstat("1\t2\n").is_err());
}
