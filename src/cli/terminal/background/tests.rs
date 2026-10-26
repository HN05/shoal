use super::*;

#[test]
fn a_screen_is_the_end_of_the_log_from_a_whole_line() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("demo.log");
    fs::write(&log, "first\nsecond\nthird\n").unwrap();
    assert_eq!(tail(&log, 1024).unwrap(), "first\nsecond\nthird\n");
    assert_eq!(tail(&log, 10).unwrap(), "third\n");
}
