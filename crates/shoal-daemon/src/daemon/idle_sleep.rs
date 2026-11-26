//! Put idle agents to sleep within seconds of their wait for the user
//! ending, which the cleanup sweep's pace would stretch.
use std::{sync::Arc, time::Duration};

use super::{
    cleanup::{LoggedFailures, SweepFailures},
    log,
    manager::Manager,
};
use crate::time::UnixTime;

/// How often Shoal looks for idle agents to put to sleep.
const SWEEP_INTERVAL: Duration = Duration::from_secs(3);

pub(super) async fn run(manager: Arc<Manager>) {
    let mut logged = LoggedFailures::default();
    loop {
        tokio::time::sleep(SWEEP_INTERVAL).await;
        let _operation = manager.gates.background_pass().await;
        let mut failures = SweepFailures::default();
        let stopped = manager
            .agents
            .stop_idle_agents(&*manager, UnixTime::now())
            .await;
        failures.step("idle agents", stopped);
        let result = failures.into_result();
        if let Some(failed) = logged.new_failures(&result) {
            log!("idle sleep: {failed}");
        }
        manager.spaces.record_idle_sleep(&*manager, result).await;
    }
}
