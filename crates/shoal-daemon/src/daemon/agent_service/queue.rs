//! The agent queue.
use shoal_agents::daemon::Queue;

use crate::daemon::manager::Manager;

impl Queue for Manager {
    fn wake_queue(&self) {
        self.agent_queue.wake();
    }
}
