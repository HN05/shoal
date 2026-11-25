//! What Shoal checks of a surface when a post brings one: that it is one
//! static surface of the pinned spec and catalog, whose references name
//! its own components and whose images name the post's attached files.
use std::collections::BTreeSet;

use anyhow::{Result, bail, ensure};
use serde_json::Value;

use super::{CATALOG_ID, Component, MAX_SURFACE_BYTES, Surface, VERSION, schema};

/// Most schema complaints a refusal lists.
const MAX_VIOLATIONS: usize = 3;

impl Surface {
    /// The surface, with the basic catalog named where it names none, once
    /// it passes every check; a refusal names what is wrong. Its images
    /// must name one of `attached`, the names of the files posted with it.
    pub fn checked(mut self, attached: &[&str]) -> Result<Self> {
        self.check_size()?;
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
    /// starts at, and a reference to a component it lacks.
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
        for component in self.components() {
            if let Some(missing) = (component.references().into_iter()).find(|id| !ids.contains(id))
            {
                bail!(
                    "component {} names component {missing}, which the surface lacks",
                    component.id
                );
            }
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

/// The targets of the Markdown images in every text of `data`, which a
/// `Text` may bind.
fn data_images(data: &Value) -> Vec<&str> {
    match data {
        Value::String(text) => super::markdown_images(text)
            .into_iter()
            .map(|(_, target)| target)
            .collect(),
        Value::Array(values) => values.iter().flat_map(data_images).collect(),
        Value::Object(fields) => fields.values().flat_map(data_images).collect(),
        _ => Vec::new(),
    }
}

/// The images `component` shows by a literal name: an `Image`'s URL, or
/// the targets of the Markdown images of a `Text`.
fn images(component: Component<'_>) -> Vec<&str> {
    match component.kind {
        "Image" => component.literal("url").into_iter().collect(),
        "Text" => (component.literal("text").into_iter())
            .flat_map(super::markdown_images)
            .map(|(_, target)| target)
            .collect(),
        _ => Vec::new(),
    }
}
