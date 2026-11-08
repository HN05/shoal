use super::*;

#[test]
fn step_names_become_single_lines_within_the_limit() {
    assert_eq!(
        step_name(" Fix\tthe\ntimeout "),
        Some("Fix the timeout".into())
    );
    assert_eq!(step_name("\n \t"), None);
    let long = step_name(&"x".repeat(600)).unwrap();
    assert!(long.len() <= crate::validate::MAX_MESSAGE_LEN && long.ends_with('…'));
    assert!(validate_line("step names", &long).is_ok());
}
