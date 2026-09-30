//! Stop one connected agent after sustained machine pressure, then let the
//! machine recover before evaluating another. Work and leases stay owned.
use std::{sync::Arc, time::Duration};
use tokio::time::Instant;

use super::workspace::Manager;

#[derive(Default)]
struct Sustained {
    since: Option<Instant>,
}

impl Sustained {
    fn observe(&mut self, now: Instant, critical: bool, seconds: u64) -> bool {
        if !critical {
            self.since = None;
            return false;
        }
        now.duration_since(*self.since.get_or_insert(now)) >= Duration::from_secs(seconds)
    }
}

pub(super) async fn run(manager: Arc<Manager>) {
    let settings = &manager.config.overload;
    if !settings.memory.enabled {
        return;
    }
    let mut memory = Sustained::default();
    loop {
        tokio::time::sleep(Duration::from_secs(settings.poll_seconds)).await;
        let critical = match crate::process::load::critical_memory(settings.memory.used_percent) {
            Ok(critical) => critical,
            Err(error) => {
                memory.since = None;
                eprintln!("overload monitor: {error:#}");
                continue;
            }
        };
        if memory.observe(Instant::now(), critical, settings.memory.sustained_seconds)
            && manager
                .stop_agent_for_overload("critical memory pressure")
                .await
        {
            memory.since = None;
            tokio::time::sleep(Duration::from_secs(settings.cooldown_seconds)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pressure_must_be_continuous_and_restart_after_recovery() {
        let start = Instant::now();
        let mut pressure = Sustained::default();
        assert!(!pressure.observe(start, true, 30));
        assert!(!pressure.observe(start + Duration::from_secs(29), true, 30));
        assert!(!pressure.observe(start + Duration::from_secs(30), false, 30));
        assert!(!pressure.observe(start + Duration::from_secs(31), true, 30));
        assert!(!pressure.observe(start + Duration::from_secs(60), true, 30));
        assert!(pressure.observe(start + Duration::from_secs(61), true, 30));
    }
}
