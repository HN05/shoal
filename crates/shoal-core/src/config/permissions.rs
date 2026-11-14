//! A space's permissions as its mode and the config give them, from
//! `[modes.NAME.permissions]`, `[permissions]` and the built-in modes'
//! defaults. Each value is ordered from the narrowest to the widest, `deny` <
//! `propose` < `gate` < `direct`.
use anyhow::Result;

pub use crate::client::config::permissions::*;
use crate::config::{
    Settings, modes,
    options::{Merge, Options},
};

/// The permissions of a space in `mode` under `settings`, before the tree
/// narrows them; a space without a recorded mode has `[permissions]` and
/// the keys' defaults. Each key resolves, highest first, from
/// `[modes.NAME.permissions]`, `[permissions]`, the built-in mode's
/// defaults, then the key's default.
pub fn resolve(settings: &Settings, mode: Option<SpaceMode>) -> Result<Permissions> {
    let (configured, built_in) = match mode {
        Some(mode) => modes::permission_layers(&settings.modes, mode)?,
        None => Default::default(),
    };
    let mut layered = PermissionsConfig::KEY_DEFAULTS;
    for layer in [built_in, settings.permissions, configured] {
        layered.merge(layer, Merge::SameLevel);
    }
    Ok(Permissions::of_mode(layered))
}

#[cfg(test)]
mod tests;
