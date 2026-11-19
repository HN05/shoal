//! Who carries out an admitted restore: the stopped agent's wrapper, which
//! waits over its link, or the daemon, which starts the agent in a new
//! place once no wrapper is left. Admission decides what a restore waits
//! for alike for both.
use shoal_ids::execution_ids::ExecutionId;
use std::collections::HashSet;

use crate::daemon::manager::Manager;

/// What restores an agent once admission lets it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Carrier {
    /// Its wrapper, which waits over its link with every process of the
    /// agent stopped.
    Wrapper,
    /// The daemon, which starts it in a new place.
    Daemon,
}

/// The wrappers that wait over their link to restore their agent, read once
/// for an admission pass.
pub(super) struct Carriers {
    links: HashSet<ExecutionId>,
}

impl Carriers {
    /// What carries out the restore of `execution`, which the daemon starts
    /// when `started_here`; none while its wrapper still stops the agent's
    /// processes or has no link, since nothing could restore it then.
    pub(super) fn of(&self, started_here: bool, execution: &ExecutionId) -> Option<Carrier> {
        if started_here {
            Some(Carrier::Daemon)
        } else if self.links.contains(execution) {
            Some(Carrier::Wrapper)
        } else {
            None
        }
    }
}

impl Manager {
    /// The carriers of the queued restores as they stand now.
    pub(super) fn carriers(&self) -> Carriers {
        Carriers {
            links: self.agent_queue.restore_links(),
        }
    }
}
