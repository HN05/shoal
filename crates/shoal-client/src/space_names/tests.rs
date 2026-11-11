use super::*;
use crate::space_ids::SpaceId;

#[test]
fn a_set_name_is_one_line_of_at_most_forty_characters() {
    let parse = |text: &str| SpaceName::parse(text).map(|name| name.to_string());
    assert_eq!(
        parse("  Plan:  summary with\u{a0}shoal done ").unwrap(),
        "Plan: summary with shoal done"
    );
    for valid in ["42", "Fix login", "修正 ログイン", "🚀 launch", "a/b: (c)"] {
        assert_eq!(parse(valid).unwrap(), valid);
    }
    assert_eq!(parse(&"é".repeat(40)).unwrap().chars().count(), 40);
    for invalid in [
        "",
        "   ",
        "two\nlines",
        "tab\there",
        "bell\u{7}",
        "right\u{202E}left",
        "isolate\u{2066}",
        &"é".repeat(41),
    ] {
        assert!(parse(invalid).is_err(), "{invalid:?}");
    }
}

#[test]
fn a_derived_name_drops_what_a_set_one_refuses_and_is_cut_with_an_ellipsis() {
    let derive = |text: &str| SpaceName::derive(text, "chat").to_string();
    assert_eq!(derive("feature/login"), "feature/login");
    assert_eq!(derive(" Fix\n the\u{202E}  login "), "Fix the login");
    assert_eq!(derive("\u{7}"), "chat");
    let long = derive(&format!("{} tail", "word ".repeat(10)));
    assert_eq!(long.chars().count(), 40);
    assert!(long.ends_with("word…"), "{long}");
}

#[test]
fn a_stored_name_reads_as_it_was_stored() {
    let long = "x".repeat(64);
    let name: SpaceName = serde_json::from_str(&format!("\"{long}\"")).unwrap();
    assert_eq!(name.as_str(), long);
    assert_eq!(serde_json::to_string(&name).unwrap(), format!("\"{long}\""));
}

#[test]
fn digits_select_a_number_and_other_text_an_id() {
    let parse = |text: &str| text.parse::<SpaceSelector>();
    let id = "0b9c3a52-6f4e-4d8a-9c1e-2f7b5d3a1e09";
    assert_eq!(parse("3").unwrap(), SpaceSelector::Number(3.into()));
    assert_eq!(parse(id).unwrap(), SpaceSelector::Id(SpaceId::from(id)));
    assert_eq!(parse("-").unwrap(), SpaceSelector::Previous);
    for neither in ["", "fix-login", "3a", "Fix login"] {
        let refused = parse(neither).unwrap_err().to_string();
        assert!(refused.contains("number or ID"), "{neither}: {refused}");
    }
    let number = parse("3").unwrap();
    assert!(number.selects(&SpaceId::from(id), 3.into()));
    assert!(!number.selects(&SpaceId::from("3"), 4.into()));
    let by_id = parse(id).unwrap();
    assert!(by_id.selects(&SpaceId::from(id), 4.into()));
    assert_eq!(by_id.given_id(), Some(&SpaceId::from(id)));
    assert_eq!(number.given_id(), None);
    let json = serde_json::to_string(&number).unwrap();
    assert_eq!(json, "\"3\"");
    assert_eq!(
        serde_json::from_str::<SpaceSelector>(&json).unwrap(),
        number
    );
    // A request's text selects what it gives, which the client checked.
    let sent = serde_json::from_str::<SpaceSelector>("\"recorded\"").unwrap();
    assert_eq!(sent, SpaceSelector::Id(SpaceId::from("recorded")));
    assert!(serde_json::from_str::<SpaceSelector>("\"\"").is_err());
}

#[test]
fn a_directory_name_keeps_portable_characters() {
    assert_eq!(directory_name("feature/login"), "feature-login");
    assert_eq!(directory_name("rust (fmt, test)"), "rust--fmt--test-");
    assert_eq!(directory_name("123"), "123");
    assert_eq!(directory_name("/_x"), "x");
    assert_eq!(directory_name("_private"), "private");
    assert_eq!(directory_name("+special"), "special");
    assert_eq!(directory_name("-_-leaf"), "leaf");
    assert_eq!(directory_name("@"), "workspace");
    assert_eq!(directory_name("Ünïcode"), "n-code");
    assert_eq!(directory_name("//"), "workspace");
    assert_eq!(directory_name(&"a".repeat(80)).len(), MAX_NAME_LEN);
    assert_eq!(suffixed_directory("fix", 1), "fix");
    let cut = suffixed_directory(&"a".repeat(MAX_NAME_LEN), 12);
    assert_eq!(cut.len(), MAX_NAME_LEN);
    assert!(cut.ends_with("-12"));
}
