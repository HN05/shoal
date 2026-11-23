//! Who a request that sends or changes something acts as: a scoped caller
//! as its own space, and the user as the space a request names as `from`,
//! as a session in it that Shoal did not start does, or else as the user.
use anyhow::Result;
use shoal_protocol::space_names::SpaceSelector;

use super::super::{manager::Manager, scope::Caller};

/// Who acts: a scoped caller, or the user acting as the space `from`
/// names, as a session in it that Shoal did not start does, or else the
/// user.
pub(super) async fn acting(
    manager: &Manager,
    caller: Option<&Caller>,
    from: Option<SpaceSelector>,
) -> Result<Option<Caller>> {
    if let Some(caller) = caller {
        return Ok(Some(caller.clone()));
    }
    let Some(from) = from else {
        return Ok(None);
    };
    // One the hub lists on another machine, as the chat of a request that
    // machine forwarded here, which only it has.
    let id = manager.spaces.space_id_anywhere(manager, &from).await?;
    Ok(Some(Caller::of_space(id)))
}
