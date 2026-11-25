//! Surfaces: the interactive interfaces a thread post offers, each one
//! static A2UI surface of the spec version Shoal pins, built from the
//! components of that version's basic catalog. `docs/design/coordination.md`
//! says how posts carry and answer them.
use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

mod checks;
mod schema;
mod shorthand;

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

    /// The agent event its action dispatches, for a button whose action is
    /// one rather than a function the renderer runs.
    fn event(&self) -> Option<&'a Map<String, Value>> {
        self.get("action")?.get("event")?.as_object()
    }
}

/// The Markdown images `![alt](target)` in `text`, each its alt text and
/// target.
fn markdown_images(text: &str) -> Vec<(&str, &str)> {
    let mut images = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("![") {
        rest = &rest[start + 2..];
        let Some((alt, after)) = rest.split_once("](") else {
            break;
        };
        let Some((target, after)) = after.split_once(')') else {
            break;
        };
        images.push((alt, target.trim()));
        rest = after;
    }
    images
}

#[cfg(test)]
mod tests;
