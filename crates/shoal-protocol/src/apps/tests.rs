use super::AppName;

#[test]
fn an_app_name_is_short_printable_text_without_a_slash() {
    assert!("quay".parse::<AppName>().is_ok());
    assert!("My App".parse::<AppName>().is_ok());
    assert!("".parse::<AppName>().is_err());
    assert!("a/b".parse::<AppName>().is_err());
    assert!("a\nb".parse::<AppName>().is_err());
    assert!("a".repeat(AppName::MAX + 1).parse::<AppName>().is_err());
}
