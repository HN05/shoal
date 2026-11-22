//! Whether Shoal wakes a space's agent for having stayed idle: an agent
//! that is waiting for something sleeps, until its next turn or for good,
//! and may set a timer that wakes it at a time.
use shoal_protocol::sleep::{Slept, WakeTimer};

use crate::connection::Prepared;
use shoal_protocol::{protocol::AgentMethod, space_names::SpaceSelector, state::Sleep};

/// Let the agent of `space` sleep as `sleep` says, replacing its timer with
/// `timer` when one is given, or with no sleep end its sleep, cancel its
/// timer and wake it.
pub fn set(
    space: SpaceSelector,
    sleep: Option<Sleep>,
    timer: Option<WakeTimer>,
) -> Prepared<Slept> {
    Prepared::new(AgentMethod::Sleep {
        space,
        sleep,
        timer,
    })
}
