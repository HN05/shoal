//! Who carries out an admitted restore: the stopped agent's waiting wrapper, or
//! the daemon once no wrapper is left.
use shoal_ids::execution_ids::ExecutionId;
use std::collections::HashSet;

use crate::Agents;

/// What restores an agent once admission lets it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Carrier {
    /// Its wrapper, which waits over its link with every process of the
    /// agent stopped.
    Wrapper,
    /// The daemon, which starts it in a new place.
    Daemon,
}

/// The wrappers that wait over their link to restore their agent, read once
/// for an admission pass.
pub struct Carriers {
    links: HashSet<ExecutionId>,
}

impl Carriers {
    /// What carries out the restore of `execution`, which the daemon starts
    /// when `started_here`; none while its wrapper still stops the agent's
    /// processes or has no link, since nothing could restore it then.
    pub fn of(&self, started_here: bool, execution: &ExecutionId) -> Option<Carrier> {
        if started_here {
            Some(Carrier::Daemon)
        } else if self.links.contains(execution) {
            Some(Carrier::Wrapper)
        } else {
            None
        }
    }
}

impl Agents {
    /// The carriers of the queued restores as they stand now.
    pub fn carriers(&self) -> Carriers {
        Carriers {
            links: self.agent_queue.restore_links(),
        }
    }
}
