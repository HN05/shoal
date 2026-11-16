//! Triggers: a `[triggers.NAME]` table pairs a source of events with a
//! launch. A source produces firings, each with a JSON input document; the
//! filter, the templates, the key and the launch read only that document,
//! whatever its source.
pub mod filter;
pub mod path;
pub mod template;
