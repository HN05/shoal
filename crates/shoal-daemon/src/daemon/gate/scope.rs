//! The scope a gate run holds: read-only, of the requesting space, so the
//! run's `shoal` commands read that space and the spaces below it and
//! change nothing. It ends with the run.
use std::{collections::BTreeMap, ffi::OsString};

use crate::client::{scope_tokens::ScopeToken, space_ids::SpaceId};
use crate::daemon::{operations::Operations, scope::Caller, workspace::Manager};

/// A read-only scope that ends when dropped.
pub(super) struct RunScope<'a> {
    operations: &'a Operations,
    token: ScopeToken,
}

impl Manager {
    /// A read-only scope of `space` for one gate run.
    pub(super) fn gate_scope(&self, space: &SpaceId) -> RunScope<'_> {
        let caller = Caller {
            space_id: space.clone(),
            execution: None,
            granted: None,
            read_only: true,
        };
        RunScope {
            operations: &self.operations,
            token: self.operations.issue(&caller),
        }
    }
}

impl RunScope<'_> {
    /// The variables that give the run's `shoal` commands this scope with
    /// this daemon.
    pub(super) fn environment(&self, paths: &crate::paths::Paths) -> BTreeMap<String, OsString> {
        BTreeMap::from([
            (crate::env::SCOPE_TOKEN.into(), self.token.expose().into()),
            (crate::env::STATE_DIR.into(), paths.state.clone().into()),
        ])
    }
}

impl Drop for RunScope<'_> {
    fn drop(&mut self) {
        self.operations.revoke(&self.token);
    }
}
