use super::*;

#[test]
fn keys_are_reported_by_name_and_an_empty_report_is_enter() {
    assert!(Key::Ctrl('d').reported_as("ctrl-d"));
    assert!(!Key::Ctrl('d').reported_as("ctrl-e"));
    assert!(Key::Enter.reported_as("enter"));
    assert!(Key::Enter.reported_as(""));
    assert!(!Key::Ctrl('d').reported_as(""));
}
