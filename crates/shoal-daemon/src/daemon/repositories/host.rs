//! The host the daemon runs on, whose headroom seed builds wait for, and
//! the agent queue that admits them by their memory.
use std::path::Path;

use anyhow::Result;
use futures_util::future::BoxFuture;
use shoal_agents::queue::{
    OneShotAdmission,
    state::{Measured, MeasuredKind},
};
use shoal_daemon_base::run_usage::{RunKey, RunUsage, expected_memory, record_run};
use shoal_ids::repository_ids::RepositoryId;
use shoal_repositories::daemon::{Host, SeedAdmission};
use tokio::sync::watch;

use crate::daemon::{
    capacity::{self, MemoryState},
    log,
    manager::Manager,
};

impl Host for Manager {
    fn clear_of_overload(&self, paths: &[&Path]) -> bool {
        let overload = &self.config().machine.overload;
        let sample = capacity::sample(paths, overload, self.work_limit());
        let disk_floor = if overload.warning.enabled {
            overload.warning.disk_free_bytes()
        } else {
            0
        };
        sample.memory == MemoryState::Healthy
            && sample.disk_free.is_some_and(|free| free >= disk_floor)
    }

    fn admit_seed_build<'a>(
        &'a self,
        repository: &'a RepositoryId,
        after_stop: bool,
    ) -> BoxFuture<'a, Result<Box<dyn SeedAdmission + 'a>>> {
        Box::pin(async move {
            let estimated = repository.clone();
            let expected = (self.store)
                .read(move |tx| expected_memory(tx, &estimated, &RunKey::SeedBuild))
                .await?;
            let measured = Measured {
                kind: MeasuredKind::SeedBuild,
                expected: expected.unwrap_or(0),
            };
            let (stop, stopped) = watch::channel(false);
            let admission = self.agents.agent_queue().request_measured(
                repository,
                None,
                measured,
                after_stop,
                move |_: &str| {
                    stop.send_replace(true);
                },
            );
            let admission = QueuedSeedBuild {
                manager: self,
                repository,
                admission,
                stopped,
            };
            Ok(Box::new(admission) as Box<dyn SeedAdmission>)
        })
    }
}

/// A seed build's admission in the daemon's agent queue.
struct QueuedSeedBuild<'a> {
    manager: &'a Manager,
    repository: &'a RepositoryId,
    admission: OneShotAdmission<'a>,
    stopped: watch::Receiver<bool>,
}

impl SeedAdmission for QueuedSeedBuild<'_> {
    fn admitted(&mut self) -> BoxFuture<'_, ()> {
        Box::pin(self.admission.admitted())
    }

    fn stopped(&self) -> watch::Receiver<bool> {
        self.stopped.clone()
    }

    fn using(&self, memory: u64) {
        self.admission.using(memory);
    }

    fn ran(&self, run: RunUsage) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            let repository = self.repository.clone();
            let recorded = (self.manager.store)
                .write(move |tx| record_run(tx, &repository, &RunKey::SeedBuild, &run))
                .await;
            if let Err(error) = recorded {
                log!("seed build of {} not recorded: {error:#}", self.repository);
            }
        })
    }
}
