//! What the answer to a request another joined machine forwarded here
//! tells its caller of this machine.
use anyhow::Result;
use shoal_protocol::{hub::Machine, scope_tokens::ScopeToken};

use super::super::manager::Manager;

/// This machine as the hub names it, for a request that `scope`, a vouch
/// of the machine that forwarded it here, shows came from another machine,
/// whose caller learns which machine answered; none for a request made
/// here.
pub(super) async fn answering_machine(
    manager: &Manager,
    scope: Option<&ScopeToken>,
) -> Result<Option<Machine>> {
    if !scope.is_some_and(|token| manager.hub.vouched_caller(token).is_some()) {
        return Ok(None);
    }
    manager.hub.this_machine(manager).await
}
