use super::*;

#[test]
fn versions_are_read_from_what_fzf_reports() {
    assert_eq!(parse_version("0.75.0 (Homebrew)\n"), Some((0, 75, 0)));
    assert_eq!(parse_version("0.44.1 (debian)"), Some((0, 44, 1)));
    assert_eq!(parse_version("0.64.0-devel (1a2b3c4)"), Some((0, 64, 0)));
    assert_eq!(parse_version("devel"), None);
    assert_eq!(parse_version(""), None);
}

#[test]
fn an_fzf_older_than_pickers_need_is_refused() {
    let refusal = supported("0.62.0 (brew)").unwrap_err().to_string();
    assert!(refusal.contains("fzf 0.62.0"), "{refusal}");
    assert!(supported("0.44.1 (debian)").is_err());
}

#[test]
fn a_supported_or_unreadable_fzf_version_passes() {
    assert!(supported("0.63.0 (brew)").is_ok());
    assert!(supported("1.0.0").is_ok());
    assert!(supported("devel").is_ok());
}
