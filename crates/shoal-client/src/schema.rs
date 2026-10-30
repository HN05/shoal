//! The API's JSON Schema, checked in at `schema/client-api.json`, and the
//! names in it that a minor version must keep.
use std::collections::BTreeSet;

use schemars::{SchemaGenerator, generate::SchemaSettings};
use serde_json::{Value, json};

use crate::{
    methods,
    records::{Record, RecordKey},
    wire::{Event, HandshakeReply, Opening, Request, Response},
};

/// The schema of every frame, each method's parameters and result, and
/// every record.
pub fn generate() -> Value {
    let mut generator = SchemaGenerator::new(SchemaSettings::draft2020_12());
    let frames = json!({
        "opening": generator.subschema_for::<Opening>(),
        "handshake_reply": generator.subschema_for::<HandshakeReply>(),
        "request": generator.subschema_for::<Request>(),
        "response": generator.subschema_for::<Response>(),
        "event": generator.subschema_for::<Event>(),
        "record": generator.subschema_for::<Record>(),
        "record_key": generator.subschema_for::<RecordKey>(),
    });
    let methods = methods::describe(&mut generator);
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "Shoal client API",
        "version": crate::VERSION,
        "frames": frames,
        "methods": methods,
        "$defs": generator.take_definitions(true),
    })
}

/// The names `schema` defines: each method, each property of a definition
/// as `Definition.property`, and each enum or constant value as
/// `Definition=value`. Removing or renaming one breaks older peers.
pub fn names(schema: &Value) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    if let Some(methods) = schema["methods"].as_object() {
        names.extend(methods.keys().map(|method| format!("method {method}")));
    }
    if let Some(definitions) = schema["$defs"].as_object() {
        for (definition, schema) in definitions {
            collect(definition, schema, &mut names);
        }
    }
    names
}

fn collect(definition: &str, schema: &Value, names: &mut BTreeSet<String>) {
    match schema {
        Value::Object(fields) => {
            for (key, value) in fields {
                match (key.as_str(), value) {
                    ("properties", Value::Object(properties)) => {
                        for (property, schema) in properties {
                            names.insert(format!("{definition}.{property}"));
                            collect(definition, schema, names);
                        }
                    }
                    ("enum", Value::Array(values)) => {
                        names.extend(
                            values
                                .iter()
                                .filter_map(Value::as_str)
                                .map(|value| format!("{definition}={value}")),
                        );
                    }
                    ("const", Value::String(value)) => {
                        names.insert(format!("{definition}={value}"));
                    }
                    _ => collect(definition, value, names),
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                collect(definition, item, names);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests;
