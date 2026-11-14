use super::*;

#[test]
fn tails_are_bounded_and_keep_the_failed_step() {
    let text = format!("{}\nJob failed\n", "🦀".repeat(TAIL_BYTES));
    let log = CheckLog::from_text("pr", "rust", &text, Some("Test".into()));
    assert!(log.truncated);
    assert!(log.text.len() <= TAIL_BYTES);
    assert_eq!(log.failed_step.as_deref(), Some("Test"));
    assert!(log.text.ends_with("Job failed\n"));
    assert!(!CheckLog::from_text("pr", "rust", "short", None).truncated);
    let job = |length| CheckLog::from_text("pr", "rust", "", None).with_job("j".repeat(length));
    assert!(job(JOB_URL_BYTES).job_url.is_some());
    assert!(job(JOB_URL_BYTES + 1).job_url.is_none());
}

#[test]
fn a_long_line_keeps_its_ends_and_leaves_room_for_the_lines_around_it() {
    let bytes = "27, 91, 50, 75, ".repeat(TAIL_BYTES);
    let text = format!("panicked at prompt.rs:67:\nhung: [{bytes}13]\nFAIL rust::prompt\n");
    let log = CheckLog::from_text("pr", "rust", &text, None);
    assert!(log.truncated);
    assert!(log.text.len() <= TAIL_BYTES);
    let lines: Vec<&str> = log.text.lines().collect();
    let [panic, shortened, fail] = lines.as_slice() else {
        panic!("{lines:?}");
    };
    assert_eq!(
        (*panic, *fail),
        ("panicked at prompt.rs:67:", "FAIL rust::prompt")
    );
    assert!(shortened.len() <= LINE_BYTES);
    assert!(shortened.starts_with("hung: [27, 91") && shortened.ends_with("75, 13]"));

    let crabs = format!("{}\n", "🦀".repeat(LINE_BYTES));
    let log = CheckLog::from_text("pr", "rust", &crabs, None);
    assert!(log.text.starts_with('🦀') && log.text.ends_with("🦀\n"));
}
