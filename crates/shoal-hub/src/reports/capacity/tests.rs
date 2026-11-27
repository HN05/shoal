use shoal_schedule::placement::snapshot::{Demand, Registered};

use super::*;

/// A capacity whose one repository kept the machine busy for `seconds`.
fn busy(seconds: u64) -> Capacity {
    let demand = Demand {
        busy_seconds: seconds,
        heavy_waiting: 0,
    };
    Capacity {
        repositories: vec![Registered {
            origin: "example.com/owner/repo".into(),
            agents: Vec::new(),
            demand,
        }],
        ..Capacity::default()
    }
}

#[test]
fn a_report_goes_once_a_repository_s_busy_time_moves_by_a_step() {
    let step = BUSY_STEP.as_secs();
    assert!(!moved(&busy(step), &busy(2 * step - 1)));
    assert!(moved(&busy(step), &busy(2 * step)));
}
