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
    // A character reference decodes into no control character shown.
    let escaped = surface(
        json!([{"id": "root", "component": "Text", "text": "![&#x1b;&#91;31m](chart.png)"}]),
        json!({}),
    );
    let lines = escaped.checked(&["chart.png"]).unwrap().layout_lines();
    assert_eq!(lines[0], "[image: [31m, attached file chart.png]");
    // Brackets nest, and a backslash escapes one, in an image's text.
    let nested = surface(
        json!([{"id": "root", "component": "Text",
            "text": "![a [b]](https://example.com/x.png) ![c \\]](https://example.com/y.png)"}]),
        json!({}),
    );
    let refused = refusal(nested.clone(), &["https://example.com/y.png"]);
    assert!(
        refused.contains("image https://example.com/x.png"),
        "{refused}"
    );
    let refused = refusal(nested, &["https://example.com/x.png"]);
    assert!(
        refused.contains("image https://example.com/y.png"),
        "{refused}"
    );
    // A definition's target may stand on the next line.
    let below = surface(
        json!([{"id": "root", "component": "Text",
            "text": "![chart][ref]\n\n[ref]:\n  https://example.com/chart.png"}]),
        json!({}),
    );
    assert!(refusal(below, &[]).contains("image https://example.com/chart.png"));
    // Labels match with their whitespace trimmed and collapsed.
    let spaced = surface(
        json!([{"id": "root", "component": "Text",
            "text": "![chart][ Latency  Chart ]\n\n[latency chart]: https://example.com/c.png"}]),
        json!({}),
    );
    assert!(refusal(spaced, &[]).contains("image https://example.com/c.png"));
    // A code block defines nothing, fenced or indented.
    for code in ["```\n[ref]: shown.png\n```", "    [ref]: shown.png"] {
        let text = format!("![chart][ref]\n\n{code}\n\n[ref]: https://example.com/d.png");
        let coded = surface(
            json!([{"id": "root", "component": "Text", "text": text}]),
            json!({}),
        );
        assert!(refusal(coded, &["shown.png"]).contains("image https://example.com/d.png"));
    }
    // A quote or a list item defines what it holds.
    for container in [
        "> [ref]: https://example.com/e.png",
        "- [ref]: https://example.com/e.png",
    ] {
        let text = format!("![chart][ref]\n\n{container}");
        let contained = surface(
            json!([{"id": "root", "component": "Text", "text": text}]),
            json!({}),
        );
        assert!(refusal(contained, &[]).contains("image https://example.com/e.png"));
    }
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

#[test]
fn images_a_text_function_takes_are_checked_too() {
    let formatted = |value: &str| {
        surface(
            json!([{"id": "root", "component": "Text", "text": {"@call": "formatString",
                "args": {"value": value}}}]),
            json!({}),
        )
    };
    let whole = formatted("![chart](https://example.com/chart.png)");
    assert!(refusal(whole.clone(), &[]).contains("image https://example.com/chart.png"));
    whole.checked(&["https://example.com/chart.png"]).unwrap();
    let split = formatted("![chart](${/url}");
    assert!(refusal(split, &[]).contains("part of an image"));
    // Data the function joins counts as its parts too.
    let joined = surface(
        json!([{"id": "root", "component": "Text", "text": {"@call": "formatString",
            "args": {"value": "${/prefix}${/suffix}"}}}]),
        json!({"prefix": "!", "suffix": "[chart](https://example.com/x.png)"}),
    );
    assert!(refusal(joined, &[]).contains("part of an image"));
    // So does data a template the function binds interpolates.
    let templated = surface(
        json!([{"id": "root", "component": "Text", "text": {"@call": "formatString",
            "args": {"value": {"@path": "/template"}}}}]),
        json!({"template": "${/prefix}${/suffix}", "prefix": "!",
            "suffix": "[x](https://example.com/x.png)"}),
    );
    assert!(refusal(templated, &[]).contains("part of an image"));
    // An image whose file a function interpolates is one too, whatever is
    // attached.
    let interpolated = surface(
        json!([{"id": "root", "component": "Text", "text": {"@call": "formatString",
            "args": {"value": "![chart](${url})"}}}]),
        json!({"url": "https://example.com/x.png"}),
    );
    assert!(refusal(interpolated, &["${url}"]).contains("part of an image"));
    // An object's keys show when a function formats it.
    let keyed = surface(
        json!([{"id": "root", "component": "Text", "text": {"@call": "formatString",
            "args": {"value": "${/obj}"}}}]),
        json!({"obj": {"![x](https://example.com/x.png)": "ok"}}),
    );
    assert!(refusal(keyed, &[]).contains("image https://example.com/x.png"));
    let plain = surface(
        json!([{"id": "root", "component": "Text", "text": {"@call": "formatString",
            "args": {"value": "Hello ${/name}"}}}]),
        json!({"name": "Ada"}),
    );
    plain.checked(&[]).unwrap();
}

#[test]
fn a_list_item_s_button_is_an_action_of_its_own_reading_its_item() {
    let listed = surface(
        json!([
            {"id": "root", "component": "List", "children": {"componentId": "row", "path": "/tasks"}},
            {"id": "row", "component": "Button", "child": "row-label",
             "action": {"event": {"name": "done", "context": {"task": {"@path": "id"}}}}},
            {"id": "row-label", "component": "Text", "text": {"@path": "title"}},
        ]),
        json!({"tasks": [{"id": 7, "title": "Write"}, {"id": 9, "title": "Ship"}]}),
    );
    let actions: Vec<(String, String)> = (listed.actions().into_iter())
        .map(|action| (action.id, action.label))
        .collect();
    assert_eq!(
        actions,
        [
            ("done#1".into(), "Write".into()),
            ("done#2".into(), "Ship".into())
        ]
    );
    let resolved = listed.resolve("done#2", serde_json::Map::new()).unwrap();
    assert_eq!(Value::Object(resolved), json!({"task": 9}));
}

#[test]
fn nested_list_items_give_their_buttons_ids_of_their_own() {
    let nested = surface(
        json!([
            {"id": "root", "component": "List", "children": {"componentId": "group", "path": "/groups"}},
            {"id": "group", "component": "List", "children": {"componentId": "row", "path": "tasks"}},
            {"id": "row", "component": "Button", "child": "row-label",
             "action": {"event": {"name": "done", "context": {"task": {"@path": "id"}}}}},
            {"id": "row-label", "component": "Text", "text": "Done"},
        ]),
        json!({"groups": [{"tasks": [{"id": 1}]}, {"tasks": [{"id": 2}, {"id": 3}]}]}),
    );
    let ids: Vec<String> = nested
        .actions()
        .into_iter()
        .map(|action| action.id)
        .collect();
    assert_eq!(ids, ["done#1.1", "done#2.1", "done#2.2"]);
    // An inner list over absolute data repeats in each outer item.
    let mut shared = nested.clone();
    shared.0["createSurface"]["components"][1]["children"]["path"] = json!("/tasks");
    shared.0["createSurface"]["dataModel"]["tasks"] = json!([{"id": 4}]);
    let ids: Vec<String> = (shared.actions().into_iter())
        .map(|action| action.id)
        .collect();
    assert_eq!(ids, ["done#1.1", "done#2.1"]);
    let resolved = nested.resolve("done#2.1", serde_json::Map::new()).unwrap();
    assert_eq!(Value::Object(resolved), json!({"task": 2}));
    let mut marked = form();
    marked.0["createSurface"]["components"][4]["action"]["event"]["name"] = json!("submit#1");
    assert!(refusal(marked, &[]).contains("holds #"));
}

#[test]
fn an_answer_that_fails_its_checks_is_refused() {
    let check = |call: &str, args: Value| json!({"condition": {"@call": call, "args": args}});
    let name = json!({"@path": "/name"});
    let form = surface(
        json!([
            {"id": "root", "component": "Column", "children": ["name", "agreed", "send"]},
            {"id": "name", "component": "TextField", "label": "Name", "value": name,
             "@checks": [
                 {"condition": {"@call": "required", "args": {"value": name}},
                  "message": "Name is required."},
                 check("length", json!({"value": name, "max": 8})),
                 check("regex", json!({"value": name, "pattern": "^[A-Z]"})),
             ]},
            {"id": "agreed", "component": "CheckBox", "label": "Agree", "value": {"@path": "/agreed"}},
            {"id": "send", "component": "Button", "child": "send-label",
             "@checks": [{"condition": {"@call": "and", "args": {"values": [
                 {"@path": "/agreed"}, {"@call": "not", "args": {"value": false}}]}}},
                 check("formatDate", json!({"value": "x", "format": "yyyy"}))],
             "action": {"event": {"name": "send",
                 "context": {"name": name, "agreed": {"@path": "/agreed"}}}}},
            {"id": "send-label", "component": "Text", "text": "Send"},
        ]),
        json!({"name": "", "agreed": false}),
    );
    let answer = |name: &str, agreed: bool| {
        form.resolve(
            "send",
            values(&[("name", json!(name)), ("agreed", json!(agreed))]),
        )
    };
    let refused = answer("", true).unwrap_err().to_string();
    assert!(refused.contains("Name is required."), "{refused}");
    assert!(answer("Adalovelace", true).is_err());
    assert!(answer("ada", true).is_err());
    // The button's own check reads the entered data too.
    assert!(answer("Ada", false).is_err());
    // A check only a renderer runs passes.
    answer("Ada", true).unwrap();
}

#[test]
fn a_check_fails_on_known_data_of_the_wrong_kind_and_reads_validation_results() {
    let count = json!({"@path": "/count"});
    let checked = |rule: Value, data: Value| {
        surface(
            json!([
                {"id": "root", "component": "Column", "children": ["count", "go"]},
                {"id": "count", "component": "TextField", "label": "Count", "value": count,
                 "@checks": [{"condition": rule}]},
                {"id": "go", "component": "Button", "child": "go-label",
                 "action": {"event": {"name": "go", "context": {"count": count}}}},
                {"id": "go-label", "component": "Text", "text": "Go"},
            ]),
            data,
        )
    };
    let numeric = json!({"@call": "numeric", "args": {"value": count, "min": 1}});
    let form = checked(numeric, json!({"count": ""}));
    let answer =
        |surface: &Surface, value: &str| surface.resolve("go", values(&[("count", json!(value))]));
    assert!(answer(&form, "abc").is_err());
    answer(&form, "3").unwrap();
    // A validation result counts as its validity inside a logical function.
    let and = json!({"@call": "and", "args": {"values": [{"@path": "/check"}, true]}});
    let not = json!({"@call": "not", "args": {"value": {"@path": "/check"}}});
    let invalid = json!({"count": "", "check": {"valid": false}});
    assert!(answer(&checked(and, invalid.clone()), "3").is_err());
    answer(&checked(not, invalid), "3").unwrap();
}

#[test]
fn a_regex_check_reads_its_pattern_as_ecmascript_does() {
    let code = json!({"@path": "/code"});
    let pattern = |pattern: &str| {
        surface(
            json!([
                {"id": "root", "component": "Column", "children": ["code", "go"]},
                {"id": "code", "component": "TextField", "label": "Code", "value": code,
                 "@checks": [{"condition": {"@call": "regex",
                     "args": {"value": code, "pattern": pattern}}}]},
                {"id": "go", "component": "Button", "child": "go-label",
                 "action": {"event": {"name": "go", "context": {
                     "code": code,
                     "known": {"@call": "required", "args": {"value": code}}}}}},
                {"id": "go-label", "component": "Text", "text": "Go"},
            ]),
            json!({"code": ""}),
        )
    };
    let answer =
        |surface: &Surface, value: &str| surface.resolve("go", values(&[("code", json!(value))]));
    let digits = pattern(r"^\d+$");
    assert!(answer(&digits, "١٢").is_err());
    // A context records a check's validation result as the catalog returns it.
    let resolved = answer(&digits, "12").unwrap();
    assert_eq!(resolved["known"], json!({"valid": true}));
    assert!(answer(&pattern(r"^\w+$"), "é").is_err());
    assert!(answer(&pattern(r"^a.b$"), "a\u{2028}b").is_err());
    answer(&pattern(r"^[\d_]+$"), "1_2").unwrap();
    // Lookahead and word boundaries work as ECMAScript's do.
    let named = pattern(r"^(?!admin$)[a-z]+$");
    assert!(answer(&named, "admin").is_err());
    answer(&named, "ada").unwrap();
    let unspaced = pattern(r"^\S+$");
    assert!(answer(&unspaced, "a\u{FEFF}b").is_err());
    answer(&pattern(r"^[\s\d]+$"), "1 2").unwrap();
    // A character escape means its code point, in a class or out of one.
    assert!(answer(&pattern(r"^[^\cJ]+$"), "a\nb").is_err());
    answer(&pattern(r"^\x41\u0042\u{43}[\u{44}\x45]\0?$"), "ABCD").unwrap();
    // A surrogate pair is the one code point it encodes.
    let smile = pattern(r"^\uD83D\uDE00$");
    assert!(answer(&smile, "plain").is_err());
    answer(&smile, "😀").unwrap();
    // A class holds its members as ECMAScript reads them.
    assert!(answer(&pattern(r"^[^a&&b]+$"), "a").is_err());
    answer(&pattern(r"^[[~]+$"), "[~").unwrap();
    answer(&pattern(r"^a[^]b$"), "a-b").unwrap();
    assert!(answer(&pattern(r"^a[]?$"), "a]").is_err());
    let bounded = pattern(r"\bcat\b");
    assert!(answer(&bounded, "concatenate").is_err());
    answer(&bounded, "a cat").unwrap();
}

#[test]
fn a_date_and_time_may_write_its_t_and_z_lowercase() {
    let meeting = surface(
        json!([
            {"id": "root", "component": "Column", "children": ["at", "book"]},
            {"id": "at", "component": "DateTimeInput", "enableDate": true, "enableTime": true,
             "min": "2026-12-10t00:00:00z", "value": {"@path": "/at"}},
            {"id": "book", "component": "Button", "child": "book-label",
             "action": {"event": {"name": "book", "context": {"at": {"@path": "/at"}}}}},
            {"id": "book-label", "component": "Text", "text": "Book"},
        ]),
        json!({"at": ""}),
    );
    let answer = |at: &str| meeting.resolve("book", values(&[("at", json!(at))]));
    assert!(answer("2026-12-01T00:00:00Z").is_err());
    answer("2026-12-11t09:00:00z").unwrap();
}

#[test]
fn a_date_input_in_a_list_item_reads_its_bounds_there() {
    let booking = surface(
        json!([
            {"id": "root", "component": "List", "children": {"componentId": "slot", "path": "/slots"}},
            {"id": "slot", "component": "Column", "children": ["day", "book"]},
            {"id": "day", "component": "DateTimeInput", "enableDate": true,
             "min": {"@path": "opens"}, "value": {"@path": "day"}},
            {"id": "book", "component": "Button", "child": "book-label",
             "action": {"event": {"name": "book", "context": {"day": {"@path": "day"}}}}},
            {"id": "book-label", "component": "Text", "text": "Book"},
        ]),
        json!({"slots": [{"opens": "2026-12-01", "day": ""}]}),
    );
    let answer = |day: &str| booking.resolve("book#1", values(&[("day", json!(day))]));
    assert!(answer("2026-11-30").is_err());
    answer("2026-12-01").unwrap();
}

#[test]
fn a_component_named_in_two_places_or_a_named_root_is_refused() {
    let shared = surface(
        json!([
            {"id": "root", "component": "Column", "children": ["open", "closed"]},
            {"id": "open", "component": "List", "children": {"componentId": "row", "path": "/open"}},
            {"id": "closed", "component": "List", "children": {"componentId": "row", "path": "/closed"}},
            {"id": "row", "component": "Text", "text": {"@path": "title"}},
        ]),
        json!({"open": [], "closed": []}),
    );
    assert!(refusal(shared, &[]).contains("named by open and closed"));
    let looping = surface(
        json!([
            {"id": "root", "component": "List", "children": {"componentId": "inner", "path": "/items"}},
            {"id": "inner", "component": "List", "children": {"componentId": "root", "path": "/items"}},
        ]),
        json!({"items": [1, 2, 3]}),
    );
    assert!(refusal(looping, &[]).contains("names root"));
}

#[test]
fn placing_a_list_that_repeats_itself_stays_within_its_bound() {
    let items: Vec<Value> = (0..2000).map(|item| json!({"id": item})).collect();
    let looping = surface(
        json!([
            {"id": "root", "component": "List", "children": {"componentId": "inner", "path": "/items"}},
            {"id": "inner", "component": "List", "children": {"componentId": "root", "path": "/items"}},
        ]),
        json!({ "items": items }),
    );
    assert!(looping.placed().len() <= MAX_PLACED);
}

#[test]
fn an_action_past_the_placing_bound_stays_answerable() {
    let tasks: Vec<Value> = (0..1500).map(|task| json!({"id": task})).collect();
    let long = surface(
        json!([
            {"id": "root", "component": "List", "children": {"componentId": "row", "path": "/tasks"}},
            {"id": "row", "component": "Button", "child": "row-label",
             "action": {"event": {"name": "done", "context": {"task": {"@path": "id"}}}}},
            {"id": "row-label", "component": "Text", "text": "Done"},
        ]),
        json!({ "tasks": tasks }),
    );
    assert!(long.actions().iter().all(|action| action.id != "done#1500"));
    let resolved = long.resolve("done#1500", serde_json::Map::new()).unwrap();
    assert_eq!(Value::Object(resolved), json!({"task": 1499}));
    assert!(long.action("done#1501").is_none());
    assert!(long.action("done").is_none());
}

#[test]
fn data_and_answers_take_newlines_whatever_their_keys() {
    let noted = surface(
        json!([
            {"id": "root", "component": "Column", "children": ["name", "save"]},
            {"id": "name", "component": "TextField", "variant": "longText", "label": "Name",
             "value": {"@path": "/name"}},
            {"id": "save", "component": "Button", "child": "save-label",
             "action": {"event": {"name": "save",
                 "context": {"name": {"@path": "/name"}, "content": "first\nsecond"}}}},
            {"id": "save-label", "component": "Text", "text": "Save"},
        ]),
        json!({"name": "Ada\nLovelace", "content": "a\nb"}),
    );
    let checked = noted.checked(&[]).unwrap();
    let answer = checked.resolve("save", values(&[("name", json!("Grace\nHopper"))]));
    assert_eq!(answer.unwrap()["name"], json!("Grace\nHopper"));
}

#[test]
fn data_may_hold_a_catalog_id_key() {
    let shop = surface(
        json!([
            {"id": "root", "component": "Button", "child": "label",
             "action": {"event": {"name": "buy", "context": {"catalogId": "products"}}}},
            {"id": "label", "component": "Text", "text": "Buy"},
        ]),
        json!({"catalogId": "products"}),
    );
    shop.clone().checked(&[]).unwrap();
    // A function call in a context names a catalog like any other.
    let mut custom = shop.clone();
    custom.0["createSurface"]["components"][0]["action"]["event"]["context"]["label"] =
        json!({"@call": "formatString", "catalogId": "custom", "args": {"value": "x"}});
    assert!(refusal(custom, &[]).contains("not custom"));
}

#[test]
fn an_option_s_label_bound_to_data_reads_the_data() {
    let sized = surface(
        json!([
            {"id": "root", "component": "Column", "children": ["size", "order"]},
            {"id": "size", "component": "ChoicePicker",
             "options": [{"label": {"@path": "/largeLabel"}, "value": "l"}],
             "value": {"@path": "/size"}},
            {"id": "order", "component": "Button", "child": "order-label",
             "action": {"event": {"name": "order", "context": {"size": {"@path": "/size"}}}}},
            {"id": "order-label", "component": "Text", "text": "Order"},
        ]),
        json!({"size": [], "largeLabel": "Large"}),
    );
    let order = sized.action("order").unwrap();
    assert_eq!(
        order.inputs[0].kind,
        InputKind::Options {
            options: vec![("l".into(), "Large".into())],
            multiple: false
        }
    );
}

#[test]
fn a_button_outside_a_list_takes_an_input_inside_it() {
    let listed = surface(
        json!([
            {"id": "root", "component": "Column", "children": ["rows", "save"]},
            {"id": "rows", "component": "List", "children": {"componentId": "row", "path": "/items"}},
            {"id": "row", "component": "Column", "children": ["name", "pick"]},
            {"id": "name", "component": "TextField", "label": "Name", "value": {"@path": "name"}},
            {"id": "pick", "component": "Button", "child": "pick-label",
             "action": {"event": {"name": "pick",
                 "context": {"index": {"@call": "@index", "args": {"offset": 1}}}}}},
            {"id": "pick-label", "component": "Text", "text": "Pick"},
            {"id": "save", "component": "Button", "child": "save-label",
             "action": {"event": {"name": "save", "context": {"first": {"@path": "/items/0/name"}}}}},
            {"id": "save-label", "component": "Text", "text": "Save"},
        ]),
        json!({"items": [{"name": "Ada"}, {"name": "Grace"}]}),
    );
    // A button outside the list takes a value for an input inside it.
    let save = listed.action("save").unwrap();
    assert_eq!(save.inputs.len(), 1);
    let resolved = listed
        .resolve("save", values(&[("first", json!("Ann"))]))
        .unwrap();
    assert_eq!(Value::Object(resolved), json!({"first": "Ann"}));
    // An item's button records its index as @index computes it.
    let resolved = listed.resolve("pick#2", serde_json::Map::new()).unwrap();
    assert_eq!(Value::Object(resolved), json!({"index": 2}));
}

#[test]
fn an_input_in_a_list_over_absolute_data_inside_another_list_is_an_input() {
    let nested = surface(
        json!([
            {"id": "root", "component": "Column", "children": ["groups", "save"]},
            {"id": "groups", "component": "List", "children": {"componentId": "group", "path": "/groups"}},
            {"id": "group", "component": "List", "children": {"componentId": "name", "path": "/items"}},
            {"id": "name", "component": "TextField", "label": "Name", "value": {"@path": "name"}},
            {"id": "save", "component": "Button", "child": "save-label",
             "action": {"event": {"name": "save", "context": {"second": {"@path": "/items/1/name"}}}}},
            {"id": "save-label", "component": "Text", "text": "Save"},
        ]),
        json!({"groups": [{}], "items": [{"name": "Ada"}, {"name": "Grace"}]}),
    );
    assert_eq!(nested.action("save").unwrap().inputs.len(), 1);
    let resolved = nested
        .resolve("save", values(&[("second", json!("Ann"))]))
        .unwrap();
    assert_eq!(Value::Object(resolved), json!({"second": "Ann"}));
}

#[test]
fn a_list_input_bound_absolutely_is_an_action_s_input() {
    let listed = surface(
        json!([
            {"id": "root", "component": "List", "children": {"componentId": "row", "path": "/items"}},
            {"id": "row", "component": "Column", "children": ["shared", "pick"]},
            {"id": "shared", "component": "TextField", "label": "Shared",
             "value": {"@path": "/shared"}},
            {"id": "pick", "component": "Button", "child": "pick-label",
             "action": {"event": {"name": "pick", "context": {
                 "shared": {"@path": "/shared"},
                 "float": {"@call": "@index", "args": {"offset": 1.0}},
                 "bound": {"@call": "@index", "args": {"offset": {"@path": "/start"}}}}}}},
            {"id": "pick-label", "component": "Text", "text": "Pick"},
        ]),
        json!({"items": [{}, {}], "shared": "all", "start": 10}),
    );
    let pick = listed.action("pick#2").unwrap();
    assert_eq!(pick.inputs.len(), 1);
    let resolved = listed
        .resolve("pick#2", values(&[("shared", json!("each"))]))
        .unwrap();
    assert_eq!(
        Value::Object(resolved),
        json!({"shared": "each", "float": 2, "bound": 11})
    );
}

/// A booking list whose date input binds `/day` absolutely, with each
/// slot's own minimum.
fn booking() -> Surface {
    surface(
        json!([
            {"id": "root", "component": "List", "children": {"componentId": "slot", "path": "/slots"}},
            {"id": "slot", "component": "Column", "children": ["day", "book"]},
            {"id": "day", "component": "DateTimeInput", "enableDate": true,
             "min": {"@path": "opens"}, "value": {"@path": "/day"}},
            {"id": "book", "component": "Button", "child": "book-label",
             "action": {"event": {"name": "book", "context": {"day": {"@path": "/day"}}}}},
            {"id": "book-label", "component": "Text", "text": "Book"},
        ]),
        json!({"day": "", "slots": [{"opens": "2026-12-01"}, {"opens": "2026-12-10"}]}),
    )
}

#[test]
fn an_input_s_bound_reads_the_value_the_answer_gives_its_data() {
    let range = surface(
        json!([
            {"id": "root", "component": "Column", "children": ["start", "end", "book"]},
            {"id": "start", "component": "DateTimeInput", "enableDate": true,
             "value": {"@path": "/start"}},
            {"id": "end", "component": "DateTimeInput", "enableDate": true,
             "min": {"@path": "/start"}, "value": {"@path": "/end"}},
            {"id": "book", "component": "Button", "child": "book-label",
             "action": {"event": {"name": "book",
                 "context": {"start": {"@path": "/start"}, "end": {"@path": "/end"}}}}},
            {"id": "book-label", "component": "Text", "text": "Book"},
        ]),
        json!({"start": "2026-12-10", "end": ""}),
    );
    let answer = |start: &str, end: &str| {
        range.resolve(
            "book",
            values(&[("start", json!(start)), ("end", json!(end))]),
        )
    };
    // The end may fall before the start the surface began with.
    answer("2026-12-01", "2026-12-05").unwrap();
    // It may not fall before the start the answer gives.
    assert!(answer("2026-12-20", "2026-12-15").is_err());
    // Nor may the end it keeps, when it gives only the start.
    let mut booked = range.clone();
    booked.0["createSurface"]["dataModel"] = json!({"start": "2026-12-01", "end": "2026-12-05"});
    let start_only = |start: &str| booked.resolve("book", values(&[("start", json!(start))]));
    assert!(start_only("2026-12-20").is_err());
    start_only("2026-12-02").unwrap();
    // An answer gives that start where the surface holds no data.
    let mut empty = range.clone();
    let body = empty.0["createSurface"].as_object_mut().unwrap();
    body.remove("dataModel");
    let answer = |start: &str, end: &str| {
        empty.resolve(
            "book",
            values(&[("start", json!(start)), ("end", json!(end))]),
        )
    };
    assert!(answer("2026-12-20", "2026-12-15").is_err());
    answer("2026-12-01", "2026-12-05").unwrap();
}

#[test]
fn every_binding_of_an_answer_reads_the_values_it_enters() {
    let person = surface(
        json!([
            {"id": "root", "component": "Column", "children": ["name", "save"]},
            {"id": "name", "component": "TextField", "label": "Name",
             "value": {"@path": "/person/name"}},
            {"id": "save", "component": "Button", "child": "save-label",
             "action": {"event": {"name": "save",
                 "context": {"name": {"@path": "/person/name"}, "person": {"@path": "/person"}}}}},
            {"id": "save-label", "component": "Text", "text": "Save"},
        ]),
        json!({"person": {"name": "Grace", "age": 36}}),
    );
    let resolved = person
        .resolve("save", values(&[("name", json!("Ada"))]))
        .unwrap();
    assert_eq!(
        Value::Object(resolved),
        json!({"name": "Ada", "person": {"name": "Ada", "age": 36}})
    );
}

#[test]
fn a_date_bound_a_function_gives_applies_where_shoal_runs_it() {
    let bounded = |min: Value| {
        surface(
            json!([
                {"id": "root", "component": "Column", "children": ["start", "day", "book"]},
                {"id": "start", "component": "DateTimeInput", "enableDate": true,
                 "value": {"@path": "/start"}},
                {"id": "day", "component": "DateTimeInput", "enableDate": true,
                 "min": min, "value": {"@path": "/day"}},
                {"id": "book", "component": "Button", "child": "book-label",
                 "action": {"event": {"name": "book",
                     "context": {"start": {"@path": "/start"}, "day": {"@path": "/day"}}}}},
                {"id": "book-label", "component": "Text", "text": "Book"},
            ]),
            json!({"start": "", "day": ""}),
        )
    };
    let answer = |surface: &Surface, day: &str| {
        surface.resolve(
            "book",
            values(&[("start", json!("2026-12-10")), ("day", json!(day))]),
        )
    };
    let format = |value: &str| json!({"@call": "formatString", "args": {"value": value}});
    let nested = json!({"@call": "formatString", "args": {"value": format("2026-12-10")}});
    for min in [format("2026-12-10"), format("${/start}"), nested] {
        let surface = bounded(min);
        assert!(answer(&surface, "2026-12-01").is_err());
        answer(&surface, "2026-12-10").unwrap();
    }
    // A bound only a renderer can work out bounds nothing.
    let unknown = bounded(json!({"@call": "formatDate",
        "args": {"value": {"@path": "/start"}, "format": "yyyy-MM-dd"}}));
    answer(&unknown, "2026-12-01").unwrap();
}

#[test]
fn a_format_string_that_grows_past_its_bounds_is_left_to_a_renderer() {
    let doubling = "${/x}${/x}";
    let mut deep = json!("${/x}");
    for _ in 0..30 {
        deep = json!({"@call": "formatString", "args": {"value": deep}});
    }
    let wide = json!({"@call": "formatString", "args": {"value": "${/big}".repeat(10)}});
    let shallow = json!({"@call": "formatString", "args": {"value":
        {"@call": "formatString", "args": {"value": "${/x}"}}}});
    let growing = surface(
        json!([
            {"id": "root", "component": "Button", "child": "label",
             "action": {"event": {"name": "go",
                 "context": {"deep": deep.clone(), "wide": wide.clone(), "shallow": shallow}}}},
            {"id": "label", "component": "Text", "text": "Go"},
        ]),
        json!({"x": doubling, "big": "y".repeat(1024)}),
    );
    let resolved = growing.resolve("go", serde_json::Map::new()).unwrap();
    assert_eq!(resolved["deep"], deep);
    assert_eq!(resolved["wide"], wide);
    assert_eq!(resolved["shallow"], json!(doubling.repeat(2)));
}

#[test]
fn a_context_s_format_string_reads_the_entered_data() {
    let call = |value: &str| json!({"@call": "formatString", "args": {"value": value}});
    let greeting = surface(
        json!([
            {"id": "root", "component": "Column", "children": ["name", "greet"]},
            {"id": "name", "component": "TextField", "label": "Name", "value": {"@path": "/name"}},
            {"id": "greet", "component": "Button", "child": "greet-label",
             "action": {"event": {"name": "greet", "context": {
                 "name": {"@path": "/name"},
                 "greeting": call("Hello ${/name}, \\${kept} ${/missing}!"),
                 "dated": call("${formatDate(value:${/when}, format:'MM-dd')}")}}}},
            {"id": "greet-label", "component": "Text", "text": "Greet"},
        ]),
        json!({"name": "Grace"}),
    );
    let resolved = greeting
        .resolve("greet", values(&[("name", json!("Ada"))]))
        .unwrap();
    assert_eq!(resolved["greeting"], json!("Hello Ada, ${kept} !"));
    // A call only a renderer can run stays as written.
    assert_eq!(
        resolved["dated"],
        call("${formatDate(value:${/when}, format:'MM-dd')}")
    );
}

/// A surface whose one text field enters `path`, with `data`.
fn entering(path: &str, data: Value) -> Surface {
    surface(
        json!([
            {"id": "root", "component": "Column", "children": ["name", "save"]},
            {"id": "name", "component": "TextField", "label": "Name", "value": {"@path": path}},
            {"id": "save", "component": "Button", "child": "save-label",
             "action": {"event": {"name": "save", "context": {"name": {"@path": path}}}}},
            {"id": "save-label", "component": "Text", "text": "Save"},
        ]),
        data,
    )
}

#[test]
fn an_answer_s_value_is_entered_where_null_or_an_empty_list_stands() {
    for (path, data) in [
        ("/person/name", json!({"person": null})),
        ("/people/0/name", json!({"people": []})),
    ] {
        let resolved = entering(path, data)
            .resolve("save", values(&[("name", json!("Ada"))]))
            .unwrap();
        assert_eq!(Value::Object(resolved), json!({"name": "Ada"}), "{path}");
    }
}

#[test]
fn a_slider_that_snaps_takes_only_its_steps() {
    let slider = surface(
        json!([
            {"id": "root", "component": "Column", "children": ["level", "save"]},
            {"id": "level", "component": "Slider", "max": 10, "steps": 2,
             "value": {"@path": "/level"}},
            {"id": "save", "component": "Button", "child": "save-label",
             "action": {"event": {"name": "save", "context": {"level": {"@path": "/level"}}}}},
            {"id": "save-label", "component": "Text", "text": "Save"},
        ]),
        json!({"level": 0}),
    );
    let answer = |level: &str| slider.resolve("save", values(&[("level", json!(level))]));
    answer("5").unwrap();
    answer("10").unwrap();
    let refused = answer("3").unwrap_err().to_string();
    assert!(refused.contains("in steps of 5"), "{refused}");
}

#[test]
fn a_number_field_may_stay_empty_where_a_slider_may_not() {
    let fields = surface(
        json!([
            {"id": "root", "component": "Column", "children": ["count", "level", "save"]},
            {"id": "count", "component": "TextField", "label": "Count", "variant": "number",
             "value": {"@path": "/count"}},
            {"id": "level", "component": "Slider", "max": 10, "value": {"@path": "/level"}},
            {"id": "save", "component": "Button", "child": "save-label",
             "action": {"event": {"name": "save",
                 "context": {"count": {"@path": "/count"}, "level": {"@path": "/level"}}}}},
            {"id": "save-label", "component": "Text", "text": "Save"},
        ]),
        json!({"count": "", "level": 5}),
    );
    let answer = |count: &str, level: &str| {
        fields.resolve(
            "save",
            values(&[("count", json!(count)), ("level", json!(level))]),
        )
    };
    let resolved = answer("", "3").unwrap();
    assert_eq!(Value::Object(resolved), json!({"count": "", "level": 3}));
    assert_eq!(answer("7", "3").unwrap()["count"], json!(7));
    assert!(answer("seven", "3").is_err());
    assert!(answer("7", "").is_err());
}

#[test]
fn an_answer_s_value_that_other_data_blocks_is_refused() {
    let blocked = entering("/person/name", json!({"person": "Grace"}));
    let refused = blocked
        .resolve("save", values(&[("name", json!("Ada"))]))
        .unwrap_err();
    assert!(
        format!("{refused:#}").contains("/person/name"),
        "{refused:#}"
    );
}

#[test]
fn a_button_in_a_list_over_absolute_data_keeps_its_outer_item_s_bounds() {
    let booking = surface(
        json!([
            {"id": "root", "component": "List", "children": {"componentId": "group", "path": "/groups"}},
            {"id": "group", "component": "Column", "children": ["day", "slots"]},
            {"id": "day", "component": "DateTimeInput", "enableDate": true,
             "min": {"@path": "opens"}, "value": {"@path": "/day"}},
            {"id": "slots", "component": "List", "children": {"componentId": "book", "path": "/items"}},
            {"id": "book", "component": "Button", "child": "book-label",
             "action": {"event": {"name": "book", "context": {"day": {"@path": "/day"}}}}},
            {"id": "book-label", "component": "Text", "text": "Book"},
        ]),
        json!({"day": "", "items": [{}],
            "groups": [{"opens": "2026-12-01"}, {"opens": "2026-12-10"}]}),
    );
    let answer = |id: &str| booking.resolve(id, values(&[("day", json!("2026-12-05"))]));
    assert!(answer("book#2.1").is_err());
    answer("book#1.1").unwrap();
}

#[test]
fn a_list_input_bound_absolutely_keeps_its_item_s_bounds() {
    let booking = booking();
    let answer = |id: &str, day: &str| booking.resolve(id, values(&[("day", json!(day))]));
    assert!(answer("book#2", "2026-12-05").is_err());
    answer("book#1", "2026-12-05").unwrap();
}

#[test]
fn a_date_answer_may_be_unset() {
    let unset = booking()
        .resolve("book#2", values(&[("day", json!(""))]))
        .unwrap();
    assert_eq!(unset["day"], json!(""));
}
