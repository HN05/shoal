//! Surfaces: the interactive interfaces a thread post offers, each one
//! static A2UI surface of the spec version Shoal pins, built from the
//! components of that version's basic catalog. `docs/design/coordination.md`
//! says how posts carry and answer them.
use std::collections::BTreeSet;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

mod actions;
mod checks;
mod dates;
mod markdown;
mod schema;
mod shorthand;
mod text;

pub use actions::{Input, InputKind, SurfaceAction};
pub use dates::{DateForm, Dates};

/// The A2UI spec version every surface is written in, which it records.
pub const VERSION: &str = "1.0";
/// The catalog of the components and functions a surface may use: A2UI's
/// basic catalog of [`VERSION`].
pub const CATALOG_ID: &str = "https://a2ui.org/specification/v1_0/catalogs/basic/catalog.json";
/// Most bytes a surface takes as JSON, which keeps a thread with a few of
/// them within one response.
pub const MAX_SURFACE_BYTES: usize = 32 * 1024;

/// One surface: an A2UI `createSurface` message that carries its
/// components and initial data model, never updated after it is posted.
/// It is what [`Surface::checked`] accepted, or a post's stored surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Surface(Value);

impl Surface {
    /// `message` as a surface, unchecked.
    pub fn new(message: Value) -> Self {
        Self(message)
    }

    /// The surface `text` holds as JSON, such as the file `shoal send
    /// --ui` reads, unchecked.
    pub fn parse(text: &str) -> Result<Self> {
        let message = serde_json::from_str(text).context("a surface is JSON")?;
        Ok(Self(message))
    }

    /// The A2UI message itself.
    pub fn as_json(&self) -> &Value {
        &self.0
    }

    /// The `createSurface` object, which holds everything else.
    fn body(&self) -> Option<&Map<String, Value>> {
        self.0.get("createSurface")?.as_object()
    }

    /// Its components, in the order it lists them.
    fn components(&self) -> impl Iterator<Item = Component<'_>> {
        (self
            .body()
            .and_then(|body| body.get("components")?.as_array()))
        .into_iter()
        .flatten()
        .filter_map(Component::of)
    }

    /// The component `id`.
    fn component(&self, id: &str) -> Option<Component<'_>> {
        self.components().find(|component| component.id == id)
    }

    /// Its components in the order they render: from the root, depth
    /// first, a list's template once for each item of its data, each once
    /// in a scope, and at most [`MAX_PLACED`] of them.
    fn placed(&self) -> Vec<Placed<'_>> {
        let mut order = Vec::new();
        let mut seen = BTreeSet::new();
        let mut next = vec![("root", String::new(), None)];
        while let Some((id, scope, item)) = next.pop() {
            let Some(component) = self.component(id) else {
                continue;
            };
            if order.len() >= MAX_PLACED || !seen.insert((id, scope.clone())) {
                continue;
            }
            let mut below: Vec<(&str, String, Option<usize>)> = (component.references())
                .into_iter()
                .filter(|child| Some(*child) != component.template().map(|(id, _)| id))
                .map(|child| (child, scope.clone(), item))
                .collect();
            if let Some((template, path)) = component.template() {
                let items = scoped(&scope, path);
                let count = self
                    .data(&items)
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len);
                below.extend((0..count).map(|n| (template, format!("{items}/{n}"), Some(n))));
            }
            next.extend(below.into_iter().rev());
            order.push(Placed {
                component,
                scope,
                item,
            });
        }
        order
    }

    /// The data `value` binds, its path read under `base` when relative,
    /// as inside a list's item; else `value` itself.
    fn value<'a>(&'a self, value: &'a Value, base: &str) -> Option<&'a Value> {
        match binding(value) {
            Some(path) => self.data(&scoped(base, path)),
            None => Some(value),
        }
    }

    /// The text `value` shows: itself, or the data it binds under `base`;
    /// a function call, which only a renderer runs, shows as an ellipsis.
    fn text(&self, value: &Value, base: &str) -> Option<String> {
        if value.get("@call").is_some() {
            return Some("…".into());
        }
        match self.value(value, base)? {
            Value::String(text) => Some(text.clone()),
            Value::Null => None,
            other => Some(other.to_string()),
        }
    }

    /// The value at the JSON pointer `path` of its initial data model.
    /// A relative path, outside a list's item, reads from the root.
    fn data(&self, path: &str) -> Option<&Value> {
        let model = self.body()?.get("dataModel")?;
        match path.trim_start_matches('/') {
            "" => Some(model),
            _ => model.pointer(&scoped("", path)),
        }
    }
}

/// Most components [`Surface::placed`] places. A component may be named
/// more than once, and a list repeats its template for each item, so
/// without a bound a small surface could place without end.
const MAX_PLACED: usize = 2000;

/// A component where it renders: under `scope`, the data path its relative
/// bindings read under, in the list item `item` when a list repeats it.
struct Placed<'a> {
    component: Component<'a>,
    scope: String,
    item: Option<usize>,
}

/// A component of a surface: its id, its type and its other properties.
#[derive(Clone, Copy)]
struct Component<'a> {
    id: &'a str,
    kind: &'a str,
    fields: &'a Map<String, Value>,
}

impl<'a> Component<'a> {
    fn of(value: &'a Value) -> Option<Self> {
        let fields = value.as_object()?;
        Some(Self {
            id: fields.get("id")?.as_str()?,
            kind: fields.get("component")?.as_str()?,
            fields,
        })
    }

    fn get(&self, field: &str) -> Option<&'a Value> {
        self.fields.get(field)
    }

    /// The literal text of the property `field`, unless it binds data or
    /// calls a function.
    fn literal(&self, field: &str) -> Option<&'a str> {
        self.get(field)?.as_str()
    }

    /// The data model path the property `field` binds to.
    fn binding(&self, field: &str) -> Option<&'a str> {
        binding(self.get(field)?)
    }

    /// The ids of the components it names directly, as its children, its
    /// child, its tabs' children, a modal's trigger and content, or the
    /// template of a list it repeats over data.
    fn references(&self) -> Vec<&'a str> {
        let single = ["child", "trigger", "content"]
            .into_iter()
            .filter_map(|field| self.literal(field));
        let children = match self.get("children") {
            Some(Value::Array(ids)) => ids.iter().filter_map(Value::as_str).collect(),
            Some(Value::Object(template)) => (template.get("componentId"))
                .and_then(Value::as_str)
                .into_iter()
                .collect(),
            _ => Vec::new(),
        };
        let tabs = (self.get("tabs").and_then(Value::as_array).into_iter())
            .flatten()
            .filter_map(|tab| tab.get("child")?.as_str());
        single.chain(children).chain(tabs).collect()
    }

    /// The template a list repeats for each item of its data: its id and
    /// the path of the data.
    fn template(&self) -> Option<(&'a str, &'a str)> {
        let template = self.get("children")?.as_object()?;
        let id = template.get("componentId")?.as_str()?;
        Some((id, template.get("path")?.as_str()?))
    }

    /// The agent event its action dispatches, for a button whose action is
    /// one rather than a function the renderer runs.
    fn event(&self) -> Option<&'a Map<String, Value>> {
        self.get("action")?.get("event")?.as_object()
    }
}

/// The data model path `value` binds to, as `{"@path": "/name"}` does.
fn binding(value: &Value) -> Option<&str> {
    value.get("@path")?.as_str()
}

/// `path` read under `base`: as it is when absolute, else below `base`.
fn scoped(base: &str, path: &str) -> String {
    match path.starts_with('/') {
        true => path.to_owned(),
        false => format!("{base}/{path}"),
    }
}

#[cfg(test)]
mod tests;
