//! What Shoal checks of a surface when a post brings one: that it is one
//! static surface of the pinned spec and catalog, whose references name
//! its own components and whose images name the post's attached files.
use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, bail, ensure};
use serde_json::Value;

use super::{CATALOG_ID, Component, MAX_SURFACE_BYTES, Surface, VERSION, markdown, schema};

/// Most schema complaints a refusal lists.
const MAX_VIOLATIONS: usize = 3;

impl Surface {
    /// The surface, with the basic catalog named where it names none, once
    /// it passes every check; a refusal names what is wrong. Its images
    /// must name one of `attached`, the names of the files posted with it.
    pub fn checked(mut self, attached: &[&str]) -> Result<Self> {
        self.check_size()?;
        check_characters(&self.0)?;
        self.check_shape()?;
        self.name_catalog();
        self.check_catalog()?;
        self.check_schema()?;
        self.check_ids()?;
        self.check_actions()?;
        self.check_images(attached)?;
        Ok(self)
    }

    fn check_size(&self) -> Result<()> {
        let bytes = serde_json::to_string(&self.0)?.len();
        ensure!(
            bytes <= MAX_SURFACE_BYTES,
            "a surface takes at most {MAX_SURFACE_BYTES} bytes of JSON, and this one {bytes}"
        );
        Ok(())
    }

    /// Refuse anything but one `createSurface` message of [`VERSION`] that
    /// carries its components, which a static surface needs.
    fn check_shape(&self) -> Result<()> {
        let version = self.0.get("version").and_then(Value::as_str);
        ensure!(
            version == Some(VERSION) && self.body().is_some(),
            "a surface is one A2UI {VERSION} createSurface message, such as \
             {{\"version\": \"{VERSION}\", \"createSurface\": {{\"surfaceId\": \"ask\", \
             \"components\": [{{\"id\": \"root\", \"component\": \"Text\", \"text\": \"Hi\"}}]}}}}"
        );
        ensure!(
            self.body()
                .is_some_and(|body| body.contains_key("components")),
            "a surface lists its components in createSurface.components"
        );
        Ok(())
    }

    fn name_catalog(&mut self) {
        if let Some(body) = self
            .0
            .get_mut("createSurface")
            .and_then(Value::as_object_mut)
        {
            body.entry("catalogId").or_insert_with(|| CATALOG_ID.into());
        }
    }

    /// Refuse another catalog, for the surface, a component or a function,
    /// and a component type the basic catalog lacks.
    fn check_catalog(&self) -> Result<()> {
        if let Some(other) = other_catalog(&self.0) {
            bail!("a surface uses only A2UI's basic catalog, {CATALOG_ID}, not {other}");
        }
        let known = schema::components();
        for component in self.components() {
            ensure!(
                known.iter().any(|kind| kind == component.kind),
                "component {} is a {}, which A2UI's basic catalog lacks; it has {}",
                component.id,
                component.kind,
                known.join(", ")
            );
        }
        Ok(())
    }

    fn check_schema(&self) -> Result<()> {
        let violations = schema::violations(&self.0, MAX_VIOLATIONS);
        ensure!(
            violations.is_empty(),
            "the surface breaks A2UI {VERSION}'s schema: {}",
            violations.join("; ")
        );
        Ok(())
    }

    /// Refuse a repeated id, a surface without the `root` its rendering
    /// starts at, a reference to a component it lacks, and a component
    /// named in two places or `root` named at all, which keeps the
    /// surface a tree.
    fn check_ids(&self) -> Result<()> {
        let mut ids = BTreeSet::new();
        for component in self.components() {
            ensure!(
                ids.insert(component.id),
                "the surface has two components {}",
                component.id
            );
        }
        ensure!(
            ids.contains("root"),
            "the surface has no component root, where its rendering starts"
        );
        let mut parents = BTreeMap::new();
        for component in self.components() {
            for child in component.references() {
                ensure!(
                    ids.contains(child),
                    "component {} names component {child}, which the surface lacks",
                    component.id
                );
                if let Some(first) = parents.insert(child, component.id) {
                    bail!(
                        "component {child} is named by {first} and {}; a component has one \
                         place in the surface, so give each place a component of its own",
                        component.id
                    );
                }
            }
        }
        if let Some(parent) = parents.get("root") {
            bail!("component {parent} names root, where the surface starts");
        }
        Ok(())
    }

    /// Refuse two buttons that dispatch the same event, which an answer
    /// names to pick one.
    fn check_actions(&self) -> Result<()> {
        let mut names = BTreeSet::new();
        for component in self.components() {
            let Some(name) = (component.event()).and_then(|event| event.get("name")?.as_str())
            else {
                continue;
            };
            ensure!(
                !name.trim().is_empty(),
                "action of {} has no name",
                component.id
            );
            ensure!(
                !name.contains('#'),
                "action {name} holds #, which marks a list item's action"
            );
            ensure!(
                names.insert(name),
                "two components dispatch the action {name}, which an answer could not tell apart"
            );
        }
        Ok(())
    }

    /// Refuse an image that names no file in `attached`: an `Image`'s, whose
    /// URL is literal so Shoal can tell, or one in Markdown, in a `Text` or
    /// in data a `Text` may bind.
    fn check_images(&self, attached: &[&str]) -> Result<()> {
        let unattached = |image: &&str| !attached.contains(image);
        for component in self.components() {
            ensure!(
                component.kind != "Image" || component.literal("url").is_some(),
                "image {} binds its url to data; an image names a file attached to the post, \
                 by its file name",
                component.id
            );
            if let Some(part) = fragments(self, component)
                .into_iter()
                .find(|part| image_part(part))
            {
                bail!(
                    "component {} builds its text from {}, part of an image; write each \
                     image whole in one text so Shoal can check its file",
                    component.id,
                    serde_json::to_string(part)?
                );
            }
            if let Some(image) = images(component).into_iter().find(unattached) {
                bail!(
                    "component {} shows the image {image}, which names no file attached to the \
                     post; attach it with --attach and name it by its file name",
                    component.id
                );
            }
        }
        let data = self.body().and_then(|body| body.get("dataModel"));
        if let Some(image) = data
            .map_or_else(Vec::new, data_images)
            .into_iter()
            .find(unattached)
        {
            bail!(
                "the surface's data shows the image {image}, which names no file attached to \
                 the post; attach it with --attach and name it by its file name"
            );
        }
        Ok(())
    }
}

/// The fields whose text names something, such as a component or an
/// action, which commands and pickers print as a word.
const NAMING: &[&str] = &[
    "id",
    "name",
    "child",
    "children",
    "trigger",
    "content",
    "componentId",
    "surfaceId",
    "catalogId",
    "@catalogId",
    "@path",
    "@call",
];

/// Refuse control characters in any text of `value`, which the terminal
/// and agents' alerts print as it is: in every key and every name, and in
/// other text all but the newlines Markdown takes.
pub(super) fn check_characters(value: &Value) -> Result<()> {
    check_text(value, false)
}

/// [`check_characters`] for `value`, a name, or held by one, when `naming`.
fn check_text(value: &Value, naming: bool) -> Result<()> {
    match value {
        Value::String(text) => ensure!(
            plain(text, !naming),
            "{} holds a control character{}",
            serde_json::to_string(text)?,
            if naming {
                "; a name takes none, nor a newline"
            } else {
                ""
            }
        ),
        Value::Array(values) => {
            for value in values {
                check_text(value, naming)?;
            }
        }
        Value::Object(fields) => {
            for (key, value) in fields {
                ensure!(
                    plain(key, false),
                    "the key {} holds a control character",
                    serde_json::to_string(key)?
                );
                check_text(value, NAMING.contains(&key.as_str()))?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Whether `text` holds no control characters, but newlines where
/// `newlines` allows them.
pub(super) fn plain(text: &str, newlines: bool) -> bool {
    !text
        .chars()
        .any(|c| c.is_control() && !(newlines && c == '\n'))
}

/// The first catalog other than the basic one that `value` names anywhere,
/// as a surface's `catalogId`, a component's `@catalogId` or a function
/// call's `catalogId`.
fn other_catalog(value: &Value) -> Option<&str> {
    match value {
        Value::Object(fields) => {
            let own = ["catalogId", "@catalogId"]
                .into_iter()
                .filter_map(|key| fields.get(key)?.as_str())
                .find(|id| *id != CATALOG_ID);
            own.or_else(|| fields.values().find_map(other_catalog))
        }
        Value::Array(values) => values.iter().find_map(other_catalog),
        _ => None,
    }
}

/// The targets of the Markdown images in every text `value` holds, such
/// as the data a `Text` may bind.
fn data_images(value: &Value) -> Vec<&str> {
    strings(value).into_iter().flat_map(targets).collect()
}

/// The images `component` shows by name: an `Image`'s URL, or the targets
/// of the Markdown images in a `Text`'s text, its literal or any text a
/// function that builds it takes.
fn images(component: Component<'_>) -> Vec<&str> {
    match component.kind {
        "Image" => component.literal("url").into_iter().collect(),
        "Text" => component.get("text").map_or_else(Vec::new, data_images),
        _ => Vec::new(),
    }
}

/// The fragments a function building `component`'s text may join: its own
/// texts and the data they interpolate, as `${/path}` or `@path` names it,
/// all of the data for a relative path. Empty for a literal text.
fn fragments<'a>(surface: &'a Surface, component: Component<'a>) -> Vec<&'a str> {
    let Some(call) = component
        .get("text")
        .filter(|text| text.get("@call").is_some())
    else {
        return Vec::new();
    };
    let own = strings(call);
    let paths = (own.iter().flat_map(|text| interpolated(text)))
        .chain(bound_paths(call))
        .collect::<Vec<_>>();
    let data = paths.into_iter().flat_map(|path| {
        let read = match path.starts_with('/') {
            true => surface.data(path),
            false => surface.data("/"),
        };
        read.map_or_else(Vec::new, strings)
    });
    own.into_iter().chain(data).collect()
}

/// A fragment that holds a bracket outside its whole images: part of an
/// image a function could join with other fragments into one.
fn image_part(fragment: &str) -> bool {
    let mut rest = fragment.to_owned();
    for image in markdown::images(fragment) {
        rest = rest.replace(image.source, "");
    }
    rest.contains(['[', ']'])
}

/// The paths the `${...}` of `text` interpolate.
fn interpolated(text: &str) -> Vec<&str> {
    let mut paths = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        rest = &rest[start + 2..];
        let Some(end) = rest.find('}') else {
            break;
        };
        paths.push(rest[..end].trim());
        rest = &rest[end + 1..];
    }
    paths
}

/// The data paths `@path` names anywhere in `value`.
fn bound_paths(value: &Value) -> Vec<&str> {
    match value {
        Value::Object(fields) => (fields.get("@path").and_then(Value::as_str).into_iter())
            .chain(fields.values().flat_map(bound_paths))
            .collect(),
        Value::Array(values) => values.iter().flat_map(bound_paths).collect(),
        _ => Vec::new(),
    }
}

/// Every text `value` holds, however deep.
fn strings(value: &Value) -> Vec<&str> {
    match value {
        Value::String(text) => vec![text.as_str()],
        Value::Array(values) => values.iter().flat_map(strings).collect(),
        Value::Object(fields) => fields.values().flat_map(strings).collect(),
        _ => Vec::new(),
    }
}

/// What the Markdown images of `text` name.
fn targets(text: &str) -> Vec<&str> {
    (markdown::images(text).into_iter())
        .map(|image| image.target)
        .collect()
}
