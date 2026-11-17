//! The keys the agent a wrapper runs reads from the terminal they share,
//! which the wrapper reports to the daemon as the user's interaction with
//! the agent: reading a key updates the terminal's access time, and the
//! agent's output does not. The daemon leaves out the keys Shoal typed.
use std::{io::IsTerminal, os::fd::AsFd, time::Duration};

use crate::{
    client::connection::CommandPaths, model::ExecutionPlan, protocol::AgentMethod, time::UnixTime,
};
use shoal_protocol::space_names::SpaceSelector;

/// How often the wrapper reads its terminal's access time, which the kernel
/// moves in steps of a few seconds.
const INTERVAL: Duration = Duration::from_secs(2);

/// Reports until dropped, when the agent's process ends.
pub(super) struct Monitor(tokio::task::JoinHandle<()>);

impl Drop for Monitor {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Whether this process runs in a terminal, which the agent it starts reads
/// keys from.
pub(super) fn has_terminal() -> bool {
    std::io::stdin().is_terminal()
}

/// When this process's terminal last had a key read from it; none when it
/// cannot tell.
fn accessed() -> Option<UnixTime> {
    let terminal = std::io::stdin().as_fd().try_clone_to_owned().ok()?;
    let accessed = std::fs::File::from(terminal)
        .metadata()
        .ok()?
        .accessed()
        .ok()?;
    Some(UnixTime::from_system_time(accessed))
}

/// Report each key the agent `plan` runs reads from this process's
/// terminal, by the time its access time moves to.
pub(super) fn start(paths: &CommandPaths, plan: &ExecutionPlan) -> Monitor {
    let paths = paths.clone();
    let (workspace, execution) = (plan.space.id.clone(), plan.id.clone());
    Monitor(tokio::spawn(async move {
        let mut last = accessed();
        loop {
            tokio::time::sleep(INTERVAL).await;
            let now = accessed();
            let Some(at) = now.filter(|_| now != last) else {
                continue;
            };
            last = now;
            let report = AgentMethod::ReportInput {
                workspace: SpaceSelector::id(&workspace),
                execution: execution.clone(),
                at,
            };
            // The agent owns the terminal, so a failed report stays quiet.
            // After an upgrade, the installed release relays it.
            crate::proxy::send_report(&paths, report).await;
        }
    }))
}
