use serde::{Deserialize, Serialize};

use super::{ConfigTarget, Forwarding, Lifetime, Target};
use crate::apps::{
    AppName,
    store::{AppScope, EntryKey, KeyPrefix, Revision, Write},
};

/// Requests of apps built on Shoal for their settings and their entries in
/// the app store.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppMethod {
    /// The table `[apps.<app>]` resolves to for `target` after every
    /// layer, none where no layer sets it. Answers with
    /// [`super::Body::AppConfig`].
    ReadAppConfig { app: AppName, target: ConfigTarget },
    /// The entry under `key`, with its bytes. Answers with
    /// [`super::Body::AppEntryPieces`], none for no entry.
    ReadAppEntry {
        app: AppName,
        scope: AppScope,
        key: EntryKey,
    },
    /// The entries whose keys start with `prefix`, without their bytes, by
    /// key. Answers with [`super::Body::AppEntries`].
    ListAppEntries {
        app: AppName,
        scope: AppScope,
        #[serde(default)]
        prefix: KeyPrefix,
    },
    /// Long-lived: write `size` bytes to the entry `write` names, from the
    /// revision it replaces. Answers with [`super::Body::Ok`] once it may
    /// take them, which the client then sends as
    /// [`crate::apps::store::AppData`] frames. Then answers with
    /// [`super::Body::AppEntry`], the entry at its new revision. Refused with
    /// `stale_revision` once another change took the entry past that
    /// revision.
    WriteAppEntry {
        app: AppName,
        scope: AppScope,
        write: Write,
        size: u64,
    },
    /// Delete the entry under `key`, which must be at `replaces`. Answers
    /// with [`super::Body::Ok`]; refused with `stale_revision` otherwise.
    DeleteAppEntry {
        app: AppName,
        scope: AppScope,
        key: EntryKey,
        replaces: Revision,
    },
    /// Long-lived: the entries whose keys start with `prefix` as
    /// [`super::Body::AppEntries`], then each change of them as
    /// [`super::Body::AppChange`], until the client hangs up.
    WatchAppEntries {
        app: AppName,
        scope: AppScope,
        #[serde(default)]
        prefix: KeyPrefix,
    },
}

impl AppMethod {
    /// Whose entries the request reaches, none for one of none.
    pub fn kept_in(&self) -> Option<AppScope> {
        match self {
            Self::ReadAppConfig { .. } => None,
            Self::ReadAppEntry { scope, .. }
            | Self::ListAppEntries { scope, .. }
            | Self::WriteAppEntry { scope, .. }
            | Self::DeleteAppEntry { scope, .. }
            | Self::WatchAppEntries { scope, .. } => Some(*scope),
        }
    }

    pub fn lifetime(&self) -> Lifetime {
        match self {
            Self::WriteAppEntry { .. } | Self::WatchAppEntries { .. } => Lifetime::Open,
            Self::ReadAppConfig { .. }
            | Self::ReadAppEntry { .. }
            | Self::ListAppEntries { .. }
            | Self::DeleteAppEntry { .. } => Lifetime::Reply,
        }
    }
}

impl AppMethod {
    pub(super) fn reads_only(&self) -> bool {
        match self {
            Self::ReadAppConfig { .. }
            | Self::ReadAppEntry { .. }
            | Self::ListAppEntries { .. }
            | Self::WatchAppEntries { .. } => true,
            Self::WriteAppEntry { .. } | Self::DeleteAppEntry { .. } => false,
        }
    }
}

impl AppMethod {
    /// An app's entries go where the app store keeps them, which is no
    /// space's machine.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::ReadAppConfig { .. }
            | Self::ReadAppEntry { .. }
            | Self::ListAppEntries { .. }
            | Self::WriteAppEntry { .. }
            | Self::DeleteAppEntry { .. }
            | Self::WatchAppEntries { .. } => Forwarding::Never,
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            Self::ReadAppConfig { .. }
            | Self::ReadAppEntry { .. }
            | Self::ListAppEntries { .. }
            | Self::WriteAppEntry { .. }
            | Self::DeleteAppEntry { .. }
            | Self::WatchAppEntries { .. } => None,
        }
    }
}
