//! The `[caches]` section: tool cache directories `[env]` names.
use shoal_states::states;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

states!(
    CacheScope {
        Shared => "shared",
    }
);

/// A tool cache directory `[env]` names with `{cache.NAME}`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Cache {
    pub scope: CacheScope,
}

pub(super) fn validate_caches(caches: &BTreeMap<String, Cache>) -> Result<()> {
    for name in caches.keys() {
        crate::validate::lowercase_name("cache", name)
            .with_context(|| format!("invalid configured cache name: {name}"))?;
    }
    Ok(())
}
