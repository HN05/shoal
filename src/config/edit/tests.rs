use super::*;

#[test]
fn edits_dotted_and_quoted_keys_without_losing_comments() {
    let source = "# policy\n[cleanup.auto]\nenabled = true # keep\nidle_minutes = 30\n";
    let result = edit(source, "cleanup.auto.enabled", Some("false")).unwrap();
    assert_eq!(result, source.replace("true", "false"));
    let result = edit(&result, "commands.\"test.unit\"", Some("['cargo', 'test']")).unwrap();
    let parsed: toml::Value = toml::from_str(&result).unwrap();
    assert_eq!(parsed["commands"]["test.unit"][0].as_str(), Some("cargo"));
    let result = edit(&result, "commands.\"test.unit\"", None).unwrap();
    assert!(!result.contains("test.unit"));
    assert!(result.contains("# keep"));
}

#[test]
fn handles_inline_tables_and_refuses_to_traverse_scalars() {
    let result = edit(
        "codex = { default_mode = 'app' }\n",
        "codex.default_mode",
        Some("cli"),
    )
    .unwrap();
    let parsed: toml::Value = toml::from_str(&result).unwrap();
    assert_eq!(parsed["codex"]["default_mode"].as_str(), Some("cli"));
    assert!(edit("default_agent = 'codex'", "default_agent.mode", Some("app")).is_err());
    assert!(edit("", "missing", None).is_err());
    assert!(edit("", "x..y", Some("1")).is_err());
}

#[test]
fn applies_changes_in_order_and_names_the_failing_key() {
    let change = |key: &str, value: Option<&str>| Change {
        key: key.into(),
        value: value.map(Into::into),
    };
    let result = apply(
        "",
        &[
            change("a.b", Some("1")),
            change("c", Some("x")),
            change("a.b", None),
        ],
    )
    .unwrap();
    let parsed: toml::Value = toml::from_str(&result).unwrap();
    assert_eq!(parsed["c"].as_str(), Some("x"));
    assert!(parsed["a"].get("b").is_none());
    let error = apply("", &[change("c", Some("x")), change("missing", None)]).unwrap_err();
    assert!(format!("{error:#}").contains("edit missing"), "{error:#}");
}
