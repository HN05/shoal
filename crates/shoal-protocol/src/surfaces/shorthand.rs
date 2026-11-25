//! The standard surface `shoal send --choice` builds: one button per
//! choice, whose actions are numbered from 1, the recommended one primary,
//! and the reason for it.
use serde_json::{Value, json};

use super::{CATALOG_ID, Surface, VERSION};

/// A part of the surface's column: the id of its top component, and its
/// components.
type Part = (String, Vec<Value>);

impl Surface {
    /// A surface offering `choices`, with `recommended`, a choice's text or
    /// number, as its primary button, else as a line of its own, and the
    /// `reason` for it.
    pub fn choices(choices: &[String], recommended: Option<&str>, reason: Option<&str>) -> Self {
        let marked = recommended.and_then(|recommended| marked(choices, recommended));
        let unmarked = recommended.filter(|_| marked.is_none());
        let mut parts: Vec<Part> = (1..)
            .zip(choices)
            .map(|(number, choice)| button(number, choice, marked == Some(number)))
            .collect();
        parts.extend(unmarked.map(|text| note("recommendation", format!("Recommended: {text}"))));
        parts.extend(reason.map(|reason| note("reason", format!("Reason: {reason}"))));
        column(parts)
    }
}

/// The number of the choice `recommended` names, by its text or number.
fn marked(choices: &[String], recommended: &str) -> Option<usize> {
    (1..).zip(choices).find_map(|(number, choice)| {
        (choice == recommended || number.to_string() == recommended).then_some(number)
    })
}

/// The button of the choice `number`, primary when `recommended`.
fn button(number: usize, choice: &str, recommended: bool) -> Part {
    let (id, label) = (format!("choice-{number}"), format!("choice-{number}-label"));
    let variant = if recommended { "primary" } else { "default" };
    let components = vec![
        json!({
            "id": id, "component": "Button", "child": label, "variant": variant,
            "action": {"event": {"name": number.to_string()}},
        }),
        json!({"id": label, "component": "Text", "text": choice}),
    ];
    (id, components)
}

/// A line of small text.
fn note(id: &str, text: String) -> Part {
    let component = json!({"id": id, "component": "Text", "text": text, "variant": "caption"});
    (id.to_owned(), vec![component])
}

/// The surface that shows `parts` in a column.
fn column(parts: Vec<Part>) -> Surface {
    let (children, parts): (Vec<String>, Vec<Vec<Value>>) = parts.into_iter().unzip();
    let root = json!({"id": "root", "component": "Column", "children": children});
    let components: Vec<Value> = std::iter::once(root)
        .chain(parts.into_iter().flatten())
        .collect();
    Surface(json!({
        "version": VERSION,
        "createSurface": {
            "surfaceId": "choices",
            "catalogId": CATALOG_ID,
            "components": components,
        },
    }))
}
