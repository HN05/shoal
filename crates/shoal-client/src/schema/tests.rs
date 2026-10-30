use std::path::PathBuf;

use super::*;
use crate::wire::major;

/// Run with this variable set to write the generated schema, which it
/// refuses while a name is missing without a major version bump.
const UPDATE: &str = "SHOAL_UPDATE_SCHEMA";

fn checked_in_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("schema/client-api.json")
}

fn checked_in() -> Value {
    let text = std::fs::read_to_string(checked_in_path()).unwrap_or_else(|error| {
        panic!(
            "read schema/client-api.json: {error}; run {UPDATE}=1 cargo test -p shoal-client schema"
        )
    });
    serde_json::from_str(&text).unwrap()
}

/// Names `checked_in` has that `generated` lost, unless the major version
/// changed between them.
fn removed_within_major(checked_in: &Value, generated: &Value) -> Vec<String> {
    let major_of = |schema: &Value| schema["version"].as_str().and_then(major);
    if major_of(checked_in) != major_of(generated) {
        return Vec::new();
    }
    names(checked_in)
        .difference(&names(generated))
        .cloned()
        .collect()
}

#[test]
fn the_checked_in_schema_is_current() {
    let generated = generate();
    if std::env::var_os(UPDATE).is_some() {
        let path = checked_in_path();
        if path.exists() {
            let removed = removed_within_major(&checked_in(), &generated);
            assert!(
                removed.is_empty(),
                "not updating the schema: removing or renaming {removed:?} needs a major version bump"
            );
        }
        let text = serde_json::to_string_pretty(&generated).unwrap() + "\n";
        std::fs::write(path, text).unwrap();
        return;
    }
    assert!(
        checked_in() == generated,
        "schema/client-api.json is out of date; run {UPDATE}=1 cargo test -p shoal-client schema"
    );
}

#[test]
fn a_minor_version_keeps_every_name() {
    let removed = removed_within_major(&checked_in(), &generate());
    assert!(
        removed.is_empty(),
        "removing or renaming {removed:?} needs a major version bump"
    );
}

#[test]
fn names_cover_methods_properties_and_values() {
    let schema = generate();
    let names = names(&schema);
    for name in [
        "method workspace.get",
        "method subscribe",
        "Workspace.branch",
        "AgentState=waiting",
        "ErrorCode=not_found",
        "Request=workspace.remove",
        "Event=removed",
    ] {
        assert!(names.contains(name), "missing {name}");
    }
}

#[test]
fn a_removed_field_or_value_is_caught_until_the_major_version_changes() {
    let old = json!({
        "version": "1.2.0",
        "methods": {"workspace.get": {}},
        "$defs": {"Workspace": {"properties": {"name": {}, "branch": {}}}, "Kind": {"enum": ["a", "b"]}},
    });
    let new = json!({
        "version": "1.3.0",
        "methods": {"workspace.get": {}},
        "$defs": {"Workspace": {"properties": {"name": {}}}, "Kind": {"enum": ["a"]}},
    });
    assert_eq!(
        removed_within_major(&old, &new),
        ["Kind=b", "Workspace.branch"]
    );
    let mut bumped = new;
    bumped["version"] = json!("2.0.0");
    assert!(removed_within_major(&old, &bumped).is_empty());
}
