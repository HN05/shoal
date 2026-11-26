use std::os::unix::process::ExitStatusExt as _;
use std::process::ExitStatus;

use super::BuildMemory;
use crate::stand_in::SeedBuilds;

#[test]
fn an_oom_kill_below_a_step_that_exits_by_itself_counts() {
    let admission = SeedBuilds::default();
    let memory = BuildMemory::new(&admission);
    // `cargo` exits 101 after the kernel killed a `rustc` below it.
    memory.step_ended(ExitStatus::from_raw(101 << 8), false, (Some(2), Some(3)));
    assert!(memory.oom_killed());
    assert!(!memory.cut_short());
}
