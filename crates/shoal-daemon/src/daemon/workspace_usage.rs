//! The sampler of what each space's processes use, on a fixed cadence and
//! at once when a workspace's containers start, with what its containers
//! use and the memory of the one-shot agent runs.
use std::sync::Arc;

use shoal_spaces::workspace_usage::{CONTAINER_READ_LIMIT, SAMPLE_INTERVAL, sample};

use super::{manager::Manager, work_memory::UnreadContainers};

pub(super) async fn run(manager: Arc<Manager>) {
    let mut unread = UnreadContainers::default();
    let usage = &manager.spaces.usage;
    loop {
        let tracked = usage.with_workspaces(|usages| usages.tracked());
        let oneshots = manager.reviews.unfinished_runs();
        let (sample, containers) = tokio::join!(
            sample(&manager.store, tracked, oneshots),
            manager.leases.container_usage(CONTAINER_READ_LIMIT)
        );
        let missed = usage.with_workspaces(|usages| usages.record(sample, containers));
        unread.observe(&manager, missed).await;
        tokio::select! {
            () = manager.leases.containers_started() => {}
            () = tokio::time::sleep(SAMPLE_INTERVAL) => {}
        }
    }
}
