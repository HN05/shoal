#[test]
fn hook_output_names_the_event_it_answers() {
    let context = "- Stop the dev server";
    let output: serde_json::Value = serde_json::from_str(&super::output(
        r#"{"hook_event_name":"PostToolUse","tool_name":"Bash"}"#,
        context,
    ))
    .unwrap();
    assert_eq!(output["hookSpecificOutput"]["hookEventName"], "PostToolUse");
    assert_eq!(output["hookSpecificOutput"]["additionalContext"], context);
    assert_eq!(super::output("", context), context);
}
