use super::*;

#[test]
fn a_given_name_may_not_be_all_digits() {
    let refused = SpaceName::parse("42").unwrap_err().to_string();
    assert!(refused.contains("space number 42"), "{refused}");
    for valid in ["fix-login", "Fix_2", "1887-select", "4a"] {
        assert_eq!(SpaceName::parse(valid).unwrap().as_str(), valid);
    }
    for invalid in ["", "-fix", "fix login", &"a".repeat(65)] {
        assert!(SpaceName::parse(invalid).is_err(), "{invalid}");
    }
}

#[test]
fn a_derived_name_that_would_be_all_digits_takes_its_kind() {
    let derive = |kind, text| SpaceName::derive(kind, text).to_string();
    assert_eq!(derive(SpaceKind::Workspace, "123"), "workspace-123");
    assert_eq!(derive(SpaceKind::Chat, "123"), "chat-123");
    assert_eq!(derive(SpaceKind::Workspace, "feature/123"), "feature-123");
    assert_eq!(derive(SpaceKind::Workspace, "/1"), "workspace-1");
    assert_eq!(derive(SpaceKind::Chat, "//"), "chat");
    let long = derive(SpaceKind::Workspace, &"9".repeat(80));
    assert_eq!(long.len(), MAX_NAME_LEN);
    assert!(long.starts_with("workspace-9"));
}

#[test]
fn a_suffix_fits_the_limit() {
    let name = SpaceName::parse("chat").unwrap();
    assert_eq!(name.suffixed(1), name);
    assert_eq!(name.suffixed(3).as_str(), "chat-3");
    let long = SpaceName::parse(&"a".repeat(MAX_NAME_LEN)).unwrap();
    let cut = long.suffixed(12);
    assert_eq!(cut.len(), MAX_NAME_LEN);
    assert!(cut.ends_with("-12"));
}

#[test]
fn reading_a_name_validates_it() {
    let name: SpaceName = serde_json::from_str("\"fix-login\"").unwrap();
    assert_eq!(serde_json::to_string(&name).unwrap(), "\"fix-login\"");
    assert!(serde_json::from_str::<SpaceName>("\"42\"").is_err());
}
