use serde_json::{Value, json};

use super::*;

/// A surface of `components` with the data model `data`.
fn surface(components: Value, data: Value) -> Surface {
    Surface::new(json!({
        "version": "1.0",
        "createSurface": {"surfaceId": "form", "components": components, "dataModel": data},
    }))
}

/// A form that asks for a name and a size, with a button that submits
/// both and one that cancels.
fn form() -> Surface {
    surface(
        json!([
            {"id": "root", "component": "Column", "children": ["title", "name", "size", "ok", "cancel"]},
            {"id": "title", "component": "Text", "text": "# Order"},
            {"id": "name", "component": "TextField", "label": "Name", "value": {"@path": "/name"}},
            {"id": "size", "component": "ChoicePicker", "label": "Size",
             "options": [{"label": "Small", "value": "s"}, {"label": "Large", "value": "l"}],
             "value": {"@path": "/size"}},
            {"id": "ok", "component": "Button", "child": "ok-label", "variant": "primary",
             "action": {"event": {"name": "submit",
                 "context": {"name": {"@path": "/name"}, "size": {"@path": "/size"}, "kind": "order"}}}},
            {"id": "ok-label", "component": "Text", "text": "Order"},
            {"id": "cancel", "component": "Button", "child": "cancel-label",
             "action": {"event": {"name": "cancel"}}},
            {"id": "cancel-label", "component": "Text", "text": "Cancel"},
        ]),
        json!({"name": "", "size": ["s"]}),
    )
}

fn values(pairs: &[(&str, Value)]) -> serde_json::Map<String, Value> {
    (pairs.iter())
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect()
}

fn refusal(surface: Surface, attached: &[&str]) -> String {
    surface.checked(attached).unwrap_err().to_string()
}

#[test]
fn a_checked_surface_names_the_basic_catalog() {
    let checked = form().checked(&[]).unwrap();
    assert_eq!(
        checked.as_json()["createSurface"]["catalogId"],
        json!(CATALOG_ID)
    );
}

#[test]
fn the_choices_shorthand_passes_its_own_checks() {
    let choices = ["Postgres".to_owned(), "SQLite".to_owned()];
    let surface = Surface::choices(&choices, Some("SQLite"), Some("Fewer moving parts"));
    assert_eq!(surface.clone().checked(&[]).unwrap(), surface);
}

#[test]
fn a_surface_of_another_shape_or_catalog_is_refused() {
    let stream = Surface::new(
        json!({"version": "1.0", "updateComponents": {"surfaceId": "s", "components": []}}),
    );
    assert!(refusal(stream, &[]).contains("createSurface message"));
    let empty = Surface::new(json!({"version": "1.0", "createSurface": {"surfaceId": "s"}}));
    assert!(refusal(empty, &[]).contains("lists its components"));
    let mut other = form();
    other.0["createSurface"]["catalogId"] = json!("example.com:charts");
    assert!(refusal(other, &[]).contains("not example.com:charts"));
}

#[test]
fn an_unknown_component_type_is_refused_by_name() {
    let chart = surface(
        json!([{"id": "root", "component": "Chart", "data": []}]),
        json!({}),
    );
    assert!(refusal(chart, &[]).contains("component root is a Chart"));
}

#[test]
fn a_component_against_the_schema_is_refused_with_where() {
    let typo = surface(
        json!([{"id": "root", "component": "Text", "txt": "Hi"}]),
        json!({}),
    );
    let refused = refusal(typo, &[]);
    assert!(refused.contains("/createSurface/components/0"), "{refused}");
}

#[test]
fn references_to_missing_components_and_repeated_actions_are_refused() {
    let dangling = surface(
        json!([{"id": "root", "component": "Card", "child": "body"}]),
        json!({}),
    );
    assert!(refusal(dangling, &[]).contains("names component body"));
    let rootless = surface(
        json!([{"id": "top", "component": "Text", "text": "Hi"}]),
        json!({}),
    );
    assert!(refusal(rootless, &[]).contains("no component root"));
    let mut twice = form();
    twice.0["createSurface"]["components"][6]["action"]["event"]["name"] = json!("submit");
    assert!(refusal(twice, &[]).contains("action submit"));
}

#[test]
fn images_name_files_attached_to_the_post() {
    let pictured = surface(
        json!([
            {"id": "root", "component": "Column", "children": ["text", "image"]},
            {"id": "text", "component": "Text", "text": "Before: ![old](before.png)"},
            {"id": "image", "component": "Image", "url": "after.svg"},
        ]),
        json!({}),
    );
    assert!(refusal(pictured.clone(), &["before.png"]).contains("image after.svg"));
    assert!(refusal(pictured.clone(), &["after.svg"]).contains("image before.png"));
    pictured.checked(&["before.png", "after.svg"]).unwrap();
}

#[test]
fn a_surface_over_its_size_is_refused() {
    let long = "x".repeat(MAX_SURFACE_BYTES);
    let large = surface(
        json!([{"id": "root", "component": "Text", "text": long}]),
        json!({}),
    );
    assert!(refusal(large, &[]).contains("at most"));
}

#[test]
fn the_shorthand_numbers_its_actions_and_marks_the_recommended_one() {
    let choices = ["Postgres".to_owned(), "SQLite".to_owned()];
    let by_text = Surface::choices(&choices, Some("SQLite"), None);
    let actions: Vec<(String, String, bool)> = (by_text.actions().into_iter())
        .map(|action| (action.id, action.label, action.recommended))
        .collect();
    assert_eq!(
        actions,
        [
            ("1".into(), "Postgres".into(), false),
            ("2".into(), "SQLite".into(), true)
        ]
    );
    assert_eq!(by_text.recommendation().as_deref(), Some("SQLite"));
    let by_number = Surface::choices(&choices, Some("1"), None);
    assert_eq!(by_number.recommendation().as_deref(), Some("Postgres"));
    let elsewhere = Surface::choices(&choices, Some("Neither"), None);
    assert_eq!(elsewhere.recommendation(), None);
    assert!(
        elsewhere
            .text_lines(None)
            .contains(&"Recommended: Neither".to_owned())
    );
}

#[test]
fn an_action_takes_the_inputs_its_context_binds() {
    let submit = form().action("submit").unwrap();
    let inputs: Vec<(&str, &InputKind)> = (submit.inputs.iter())
        .map(|input| (input.key.as_str(), &input.kind))
        .collect();
    assert_eq!(
        inputs,
        [
            ("name", &InputKind::Text),
            (
                "size",
                &InputKind::Options {
                    options: vec![("s".into(), "Small".into()), ("l".into(), "Large".into())],
                    multiple: false
                }
            ),
        ]
    );
    assert_eq!(submit.label, "Order");
}

#[test]
fn an_answer_resolves_the_context_with_the_values_given() {
    let given = values(&[("name", json!("Ada")), ("size", json!("l"))]);
    let resolved = form().resolve("submit", given).unwrap();
    assert_eq!(
        Value::Object(resolved),
        json!({"name": "Ada", "size": ["l"], "kind": "order"})
    );
    let defaults = form().resolve("submit", serde_json::Map::new()).unwrap();
    assert_eq!(
        Value::Object(defaults),
        json!({"name": "", "size": ["s"], "kind": "order"})
    );
}

#[test]
fn an_answer_is_refused_for_what_the_surface_does_not_take() {
    let refused = |id: &str, given: &[(&str, Value)]| {
        form().resolve(id, values(given)).unwrap_err().to_string()
    };
    assert!(refused("delete", &[]).contains("it has submit, cancel"));
    assert!(refused("cancel", &[("name", json!("Ada"))]).contains("takes no values"));
    assert!(refused("submit", &[("colour", json!("red"))]).contains("it takes name, size"));
    assert!(refused("submit", &[("size", json!("xl"))]).contains("not \"xl\""));
    assert!(refused("submit", &[("size", json!(["s", "l"]))]).contains("takes one of"));
}

#[test]
fn values_given_as_text_take_their_input_type() {
    let settings = surface(
        json!([
            {"id": "root", "component": "Column", "children": ["notify", "volume", "save"]},
            {"id": "notify", "component": "CheckBox", "label": "Notify", "value": {"@path": "/notify"}},
            {"id": "volume", "component": "Slider", "max": 10, "value": {"@path": "/volume"}},
            {"id": "save", "component": "Button", "child": "save-label",
             "action": {"event": {"name": "save",
                 "context": {"notify": {"@path": "/notify"}, "volume": {"@path": "/volume"}}}}},
            {"id": "save-label", "component": "Text", "text": "Save"},
        ]),
        json!({"notify": false, "volume": 3}),
    );
    let given = values(&[("notify", json!("true")), ("volume", json!("7"))]);
    let resolved = settings.resolve("save", given).unwrap();
    assert_eq!(
        Value::Object(resolved),
        json!({"notify": true, "volume": 7.0})
    );
    let loud = settings.resolve("save", values(&[("volume", json!("11"))]));
    assert!(loud.unwrap_err().to_string().contains("from 0 to 10"));
}

#[test]
fn the_text_shows_the_inputs_and_how_to_answer_each_action() {
    let lines = form().text_lines(Some("shoal send 3.2"));
    assert_eq!(
        lines,
        [
            "# Order",
            "Name: …",
            "Size:",
            "  (•) Small",
            "  ( ) Large",
            "[Order]",
            "[Cancel]",
            "Actions:",
            "  submit Order (recommended), takes name (text), size (one of s|l): \
             shoal send 3.2 --action submit --value name=… --value size=…",
            "  cancel Cancel: shoal send 3.2 --action cancel",
        ]
    );
}

#[test]
fn the_text_repeats_a_lists_template_for_each_item_and_shows_images() {
    let list = surface(
        json!([
            {"id": "root", "component": "Card", "child": "items"},
            {"id": "items", "component": "List",
             "children": {"componentId": "item", "path": "/items"}},
            {"id": "item", "component": "Text", "text": {"@path": "name"}},
            {"id": "chart", "component": "Text", "text": "![Latency](latency.svg)"},
        ]),
        json!({"items": [{"name": "First"}, {"name": "Second"}]}),
    );
    assert_eq!(list.text_lines(None), ["    First", "    Second"]);
    let pictured = surface(
        json!([{"id": "root", "component": "Text", "text": "See ![Latency](latency.svg)"}]),
        json!({}),
    );
    assert_eq!(
        pictured.text_lines(None),
        ["See [image: Latency, attached file latency.svg]"]
    );
}

#[test]
fn an_action_s_inputs_come_in_the_order_they_render() {
    let ordered = surface(
        json!([
            {"id": "root", "component": "Column", "children": ["size", "note", "go"]},
            {"id": "size", "component": "TextField", "label": "Size", "value": {"@path": "/size"}},
            {"id": "note", "component": "TextField", "label": "Note", "value": {"@path": "/note"}},
            {"id": "go", "component": "Button", "child": "go-label",
             "action": {"event": {"name": "go",
                 "context": {"a-note": {"@path": "/note"}, "b-size": {"@path": "/size"}}}}},
            {"id": "go-label", "component": "Text", "text": "Go"},
        ]),
        json!({}),
    );
    let go = ordered.action("go").unwrap();
    let keys: Vec<&str> = go.inputs.iter().map(|input| input.key.as_str()).collect();
    assert_eq!(keys, ["b-size", "a-note"]);
}
