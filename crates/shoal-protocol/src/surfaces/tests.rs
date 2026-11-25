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
