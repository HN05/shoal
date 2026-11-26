//! The scope a one-shot run holds, of the space it serves. It ends with the
//! run.
use std::{collections::BTreeMap, ffi::OsString};

use shoal_daemon_base::callers::{Access, Caller};
use shoal_daemon_base::operations::Operations;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::paths::Paths;
use shoal_protocol::scope_tokens::ScopeToken;

use crate::env;

/// A run's scope, which ends when dropped.
pub(crate) struct RunScope<'a> {
    operations: &'a Operations,
    token: ScopeToken,
}

impl<'a> RunScope<'a> {
    /// A scope of `space` with `access` for one run.
    pub(crate) fn of(operations: &'a Operations, space: &SpaceId, access: Access) -> Self {
        let caller = Caller {
            access,
            ..Caller::of_space(space.clone())
        };
        let token = operations.issue(&caller);
        Self { operations, token }
    }

    /// The variables that give the run's `shoal` commands this scope with
    /// this daemon.
    pub(crate) fn environment(&self, paths: &Paths) -> BTreeMap<String, OsString> {
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
