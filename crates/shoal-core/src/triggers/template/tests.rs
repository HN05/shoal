use serde_json::json;

use super::Template;

#[test]
fn renders_lookups_as_text_or_json() {
    let input = json!({
        "payload": {
            "issue": { "number": 42, "title": "Crash" },
            "labels": [{ "name": "bug" }, { "name": "p1" }],
        },
    });
    let render = |text: &str| text.parse::<Template>().unwrap().render(&input);
    assert_eq!(
        render("#{{ payload.issue.number }}: {{payload.issue.title}}").unwrap(),
        "#42: Crash"
    );
    assert_eq!(render("{{ payload.labels.1.name }}").unwrap(), "p1");
    assert_eq!(
        render("{{ payload.labels.0 }}").unwrap(),
        r#"{"name":"bug"}"#
    );
    let missing = render("since {{ previous_at }}").unwrap_err();
    assert_eq!(missing.0.to_string(), "previous_at");
}

#[test]
fn refuses_what_is_not_a_lookup() {
    for text in [
        "{{ a",
        "a }}",
        "a }} b {{ c }}",
        "{{ }}",
        "{{ a b }}",
        "{{ labels[].name }}",
    ] {
        assert!(text.parse::<Template>().is_err(), "{text}");
    }
    assert!("plain".parse::<Template>().unwrap().is_static());
    assert!(!"{{ a }}".parse::<Template>().unwrap().is_static());
}
