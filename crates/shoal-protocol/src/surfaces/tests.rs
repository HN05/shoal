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
fn images_from_data_name_attached_files_too() {
    let bound = surface(
        json!([{"id": "root", "component": "Image", "url": {"@path": "/image"}}]),
        json!({"image": "https://example.com/a.png"}),
    );
    assert!(refusal(bound, &[]).contains("binds its url"));
    let listed = surface(
        json!([
            {"id": "root", "component": "List", "children": {"componentId": "item", "path": "/items"}},
            {"id": "item", "component": "Text", "text": {"@path": "note"}},
        ]),
        json!({"items": [{"note": "See ![chart](https://example.com/b.png)"}]}),
    );
    assert!(refusal(listed.clone(), &[]).contains("image https://example.com/b.png"));
    listed.checked(&["https://example.com/b.png"]).unwrap();
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
    // Empty text, as `--value size=` gives it, selects no option.
    let cleared = form()
        .resolve("submit", values(&[("size", json!(""))]))
        .unwrap();
    assert_eq!(cleared["size"], json!([]));
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
        json!({"notify": true, "volume": 7})
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

#[test]
fn control_characters_are_refused_but_newlines_in_text() {
    let escaped = surface(
        json!([{"id": "root", "component": "Text", "text": "Red \u{1b}[31m"}]),
        json!({}),
    );
    assert!(refusal(escaped, &[]).contains("control character"));
    let mut named = form();
    named.0["createSurface"]["components"][4]["action"]["event"]["name"] = json!("sub\nmit");
    assert!(refusal(named, &[]).contains("a name takes none"));
    let lines = surface(
        json!([{"id": "root", "component": "Text", "text": "First\nSecond"}]),
        json!({}),
    );
    lines.checked(&[]).unwrap();
    let typed = form().resolve("submit", values(&[("name", json!("Ada\u{7}"))]));
    assert!(typed.unwrap_err().to_string().contains("control character"));
    assert!(
        form()
            .resolve("sub\u{1b}mit", serde_json::Map::new())
            .is_err()
    );
}

#[test]
fn a_surface_that_names_its_components_again_draws_within_a_bound() {
    let mut components =
        vec![json!({"id": "root", "component": "Column", "children": ["c0", "c0"]})];
    for level in 0..40 {
        let next = format!("c{}", level + 1);
        components.push(json!({"id": format!("c{level}"), "component": "Column",
            "children": [next.clone(), next]}));
    }
    components.push(json!({"id": "c40", "component": "Text", "text": "leaf"}));
    let doubling = surface(Value::Array(components), json!({}));
    let lines = doubling.layout_lines();
    assert!(lines.len() <= 2 * text::MAX_DRAWN, "{}", lines.len());
    assert!(lines.last().unwrap().contains("--json"), "{lines:?}");
}

#[test]
fn a_date_input_takes_date_bounds() {
    let dated = surface(
        json!([{"id": "root", "component": "DateTimeInput", "enableDate": true,
            "min": "2026-11-26", "max": "2026-12-31T12:00:00Z", "value": {"@path": "/when"}}]),
        json!({"when": ""}),
    );
    dated.checked(&[]).unwrap();
    let vague = surface(
        json!([{"id": "root", "component": "DateTimeInput", "enableDate": true,
            "min": "tomorrow", "value": {"@path": "/when"}}]),
        json!({"when": ""}),
    );
    assert!(refusal(vague, &[]).contains("schema"));
}

#[test]
fn a_date_answer_takes_the_input_s_form_and_bounds() {
    let booking = surface(
        json!([
            {"id": "root", "component": "Column", "children": ["day", "book"]},
            {"id": "day", "component": "DateTimeInput", "enableDate": true,
             "min": "2026-11-26", "max": "2026-12-31", "value": {"@path": "/day"}},
            {"id": "book", "component": "Button", "child": "book-label",
             "action": {"event": {"name": "book", "context": {"day": {"@path": "/day"}}}}},
            {"id": "book-label", "component": "Text", "text": "Book"},
        ]),
        json!({"day": ""}),
    );
    let answer = |day: &str| booking.resolve("book", values(&[("day", json!(day))]));
    assert_eq!(answer("2026-12-01").unwrap()["day"], json!("2026-12-01"));
    for refused in ["garbage", "2026-02-30", "14:30", "2026-11-25", "2027-01-01"] {
        assert!(answer(refused).is_err(), "{refused}");
    }
}

#[test]
fn a_date_bound_compares_moments_and_reads_bound_data() {
    let meeting = surface(
        json!([
            {"id": "root", "component": "Column", "children": ["at", "meet"]},
            {"id": "at", "component": "DateTimeInput", "enableDate": true, "enableTime": true,
             "min": {"@path": "/earliest"}, "value": {"@path": "/at"}},
            {"id": "meet", "component": "Button", "child": "meet-label",
             "action": {"event": {"name": "meet", "context": {"at": {"@path": "/at"}}}}},
            {"id": "meet-label", "component": "Text", "text": "Meet"},
        ]),
        json!({"at": "", "earliest": "2026-11-26T12:00:00Z"}),
    );
    let answer = |at: &str| meeting.resolve("meet", values(&[("at", json!(at))]));
    // 13:00 at UTC+2 is 11:00 UTC, before the bound the data model sets.
    assert!(answer("2026-11-26T13:00:00+02:00").is_err());
    assert!(answer("2026-11-26T11:59:00Z").is_err());
    answer("2026-11-26T15:00:00+02:00").unwrap();
    answer("2026-11-27T08:00:00Z").unwrap();
}

#[test]
fn context_data_no_input_enters_is_taken_as_it_is() {
    let counted = surface(
        json!([
            {"id": "root", "component": "Column", "children": ["note", "go"]},
            {"id": "note", "component": "TextField", "label": "Note", "value": {"@path": "/note"}},
            {"id": "go", "component": "Button", "child": "go-label",
             "action": {"event": {"name": "go",
                 "context": {"count": {"@path": "/count"}, "note": {"@path": "/note"}}}}},
            {"id": "go-label", "component": "Text", "text": "Go"},
        ]),
        json!({"count": 3, "note": ""}),
    );
    let go = counted.action("go").unwrap();
    let keys: Vec<&str> = go.inputs.iter().map(|input| input.key.as_str()).collect();
    assert_eq!(keys, ["note"]);
    let resolved = counted.resolve("go", serde_json::Map::new()).unwrap();
    assert_eq!(Value::Object(resolved), json!({"count": 3, "note": ""}));
    assert!(
        counted
            .resolve("go", values(&[("count", json!("4"))]))
            .is_err()
    );
}

#[test]
fn images_by_reference_name_attached_files_too() {
    let referenced = surface(
        json!([{"id": "root", "component": "Text",
            "text": "See ![chart][latency] and ![Map]\n\n[latency]: https://example.com/a.png\n[map]: map.png"}]),
        json!({}),
    );
    assert!(refusal(referenced.clone(), &["map.png"]).contains("image https://example.com/a.png"));
    let attached = ["https://example.com/a.png", "map.png"];
    let lines = referenced
        .clone()
        .checked(&attached)
        .unwrap()
        .layout_lines();
    assert_eq!(
        lines[0],
        "See [image: chart, attached file https://example.com/a.png] and [image: Map, attached file map.png]"
    );
    // A bracket without a definition is text, not an image.
    let plain = surface(
        json!([{"id": "root", "component": "Text", "text": "![not an image]"}]),
        json!({}),
    );
    plain.checked(&[]).unwrap();
}

#[test]
fn a_relative_binding_reads_from_the_root_of_the_data() {
    let relative = surface(
        json!([
            {"id": "root", "component": "Column", "children": ["note", "go"]},
            {"id": "note", "component": "TextField", "label": "Note", "value": {"@path": "/note"}},
            {"id": "go", "component": "Button", "child": "go-label",
             "action": {"event": {"name": "go",
                 "context": {"count": {"@path": "count"}, "note": {"@path": "note"}}}}},
            {"id": "go-label", "component": "Text", "text": "Go"},
        ]),
        json!({"count": 3, "note": "draft"}),
    );
    let go = relative.action("go").unwrap();
    assert_eq!(go.inputs.len(), 1);
    assert_eq!(go.inputs[0].initial, Some(json!("draft")));
    let resolved = relative.resolve("go", serde_json::Map::new()).unwrap();
    assert_eq!(
        Value::Object(resolved),
        json!({"count": 3, "note": "draft"})
    );
}
