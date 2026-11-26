use super::*;

/// Small, Medium and Large, by value and label.
fn sizes() -> Vec<(String, String)> {
    ["Small", "Medium", "Large"]
        .into_iter()
        .map(|size| (size.to_lowercase(), size.to_owned()))
        .collect()
}

fn ids(entries: &Entries) -> Vec<&str> {
    entries.iter().map(|(id, _)| id.as_str()).collect()
}

#[test]
fn one_option_picker_starts_on_the_surface_s_choice() {
    let (entries, selected) = option_entries(&sizes(), &["large".to_owned()], false);
    assert_eq!(ids(&entries), ["option:2", "option:0", "option:1", "none"]);
    assert!(selected.is_empty());
}

#[test]
fn an_empty_choice_starts_on_picking_none() {
    for multiple in [false, true] {
        let (entries, selected) = option_entries(&sizes(), &[], multiple);
        assert_eq!(ids(&entries)[0], "none", "multiple: {multiple}");
        assert!(selected.is_empty());
    }
}

#[test]
fn several_options_start_selected_as_the_surface_holds_them() {
    let current = ["small".to_owned(), "large".to_owned()];
    let (entries, selected) = option_entries(&sizes(), &current, true);
    assert_eq!(ids(&entries), ["option:0", "option:1", "option:2", "none"]);
    assert_eq!(selected, ["option:0", "option:2"]);
}

#[test]
fn picking_none_clears_a_selection() {
    let picked = ["option:0".to_owned(), "none".to_owned()];
    assert_eq!(picked_values(&picked, &sizes()), serde_json::json!([]));
    let picked = ["option:0".to_owned(), "option:2".to_owned()];
    assert_eq!(
        picked_values(&picked, &sizes()),
        serde_json::json!(["small", "large"])
    );
}
