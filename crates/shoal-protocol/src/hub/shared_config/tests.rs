use super::*;

#[test]
fn a_file_name_reads_back_what_it_writes() {
    for (text, name) in [
        ("shared.toml", FileName::Config),
        ("modes/review.md", FileName::Mode("review".into())),
        ("rules/tests_first.md", FileName::Rule("tests_first".into())),
    ] {
        assert_eq!(text.parse::<FileName>().unwrap(), name);
        assert_eq!(serde_json::to_value(&name).unwrap(), text);
    }
}

#[test]
fn a_file_name_names_no_other_file() {
    for text in [
        "config.toml",
        "modes/../../config.toml.md",
        "modes/a/b.md",
        "modes/.md",
        "rules/review.txt",
        "shared/modes/review.md",
    ] {
        assert!(text.parse::<FileName>().is_err(), "{text}");
        assert!(serde_json::from_value::<FileName>(text.into()).is_err());
    }
}
