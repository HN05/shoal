use super::*;

#[test]
fn a_command_key_records_its_name_or_its_arguments() {
    let named = CommandKey::Named("test".into());
    let argv = CommandKey::Argv(vec!["cargo".into(), "test".into()]);
    assert_eq!(
        serde_json::to_string(&named).unwrap(),
        r#"{"named":"test"}"#
    );
    assert_eq!(
        serde_json::to_string(&argv).unwrap(),
        r#"{"argv":["cargo","test"]}"#
    );
    assert_eq!(named.to_string(), "test");
    assert_eq!(argv.to_string(), "cargo test");
}
