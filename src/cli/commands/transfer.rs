//! Moving workspaces to another Shoal: `shoal export` removes a workspace and
//! writes it to a file, and `shoal import` opens it again from one.
mod export;
mod import;

pub(super) use export::{export, export_all};
pub(super) use import::{ImportOptions, import};

/// The file name that reads or writes the standard streams.
const STANDARD_STREAM: &str = "-";
