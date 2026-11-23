//! The scope a gate run holds: read-only, of the requesting space, so the
//! run's `shoal` commands read that space and the spaces below it and
//! change nothing. It ends with the run.
use shoal_daemon_base::callers::Access;
use std::{collections::BTreeMap, ffi::OsString};

use shoal_daemon_base::callers::Caller;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::paths::Paths;
use shoal_protocol::scope_tokens::ScopeToken;

use shoal_daemon_base::operations::Operations;

use crate::env;

/// A read-only scope that ends when dropped.
pub(super) struct RunScope<'a> {
    operations: &'a Operations,
    token: ScopeToken,
}

impl<'a> RunScope<'a> {
    /// A read-only scope of `space` for one gate run.
    pub(super) fn of(operations: &'a Operations, space: &SpaceId) -> Self {
        let caller = Caller {
            access: Access::ReadOnly,
            ..Caller::of_space(space.clone())
        };
        let token = operations.issue(&caller);
        Self { operations, token }
    }

    /// The variables that give the run's `shoal` commands this scope with
    /// this daemon.
    pub(super) fn environment(&self, paths: &Paths) -> BTreeMap<String, OsString> {
        BTreeMap::from([
            (env::SCOPE_TOKEN.into(), self.token.expose().into()),
            (env::STATE_DIR.into(), paths.state.clone().into()),
        ])
    }
}

impl Drop for RunScope<'_> {
    fn drop(&mut self) {
        self.operations.revoke(&self.token);
    }
}
