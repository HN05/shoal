//! The A2UI schema of [`super::VERSION`] and its basic catalog, vendored
//! unchanged from the A2UI repository (Apache-2.0, see `a2ui_v1_0/LICENSE`),
//! and the validator built from them.
use std::sync::LazyLock;

use jsonschema::{Draft, Registry, Validator};
use serde_json::{Value, json};

const BASE: &str = "https://a2ui.org/specification/v1_0/";

const MESSAGES: &str = include_str!("a2ui_v1_0/agent_to_renderer.json");
const COMMON_TYPES: &str = include_str!("a2ui_v1_0/common_types.json");
const BASIC_CATALOG: &str = include_str!("a2ui_v1_0/basic_catalog.json");

/// The validator of a `createSurface` message whose components and
/// functions are the basic catalog's. The messages schema names its
/// catalog `catalog.json` beside it, and the catalog its common types
/// beside itself, so both are registered where those names resolve.
static VALIDATOR: LazyLock<Validator> = LazyLock::new(|| {
    let parse = |text| serde_json::from_str::<Value>(text).expect("vendored schema parses");
    let registry = Registry::new()
        .add(format!("{BASE}agent_to_renderer.json"), parse(MESSAGES))
        .and_then(|registry| registry.add(format!("{BASE}common_types.json"), parse(COMMON_TYPES)))
        .and_then(|registry| {
            registry.add(
                format!("{BASE}catalogs/basic/common_types.json"),
                parse(COMMON_TYPES),
            )
        })
        .and_then(|registry| registry.add(format!("{BASE}catalog.json"), parse(BASIC_CATALOG)))
        .and_then(|registry| registry.prepare())
        .expect("vendored schemas register");
    let schema =
        json!({"$ref": format!("{BASE}agent_to_renderer.json#/$defs/CreateSurfaceMessage")});
    jsonschema::options()
        .with_draft(Draft::Draft202012)
        .with_registry(&registry)
        .build(&schema)
        .expect("vendored schema builds")
});

/// The component types of the basic catalog.
static COMPONENTS: LazyLock<Vec<String>> = LazyLock::new(|| {
    let catalog: Value = serde_json::from_str(BASIC_CATALOG).expect("vendored catalog parses");
    (catalog["components"].as_object().into_iter())
        .flat_map(|components| components.keys().cloned())
        .collect()
});

/// The schema's complaints about `message`, each with where in it, at most
/// `limit` of them.
pub(super) fn violations(message: &Value, limit: usize) -> Vec<String> {
    (VALIDATOR.iter_errors(message).take(limit))
        .map(|error| format!("{} at {}", error, error.instance_path()))
        .collect()
}

/// The component types a surface may use.
pub(super) fn components() -> &'static [String] {
    &COMPONENTS
}
