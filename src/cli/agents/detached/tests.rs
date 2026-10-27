use std::{
    os::unix::process::CommandExt,
    time::{Duration, Instant},
};

use super::*;

#[test]
fn stopping_a_session_stops_the_agent_it_started() {
    let directory = tempfile::tempdir().unwrap();
    let agent_pid = directory.path().join("agent.pid");
    let session = std::process::Command::new("sh")
        .arg("-c")
        .arg("sleep 600 & echo $! > \"$1\"; wait")
        .arg("session")
        .arg(&agent_pid)
        .process_group(0)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let agent = loop {
        if let Ok(pid) = std::fs::read_to_string(&agent_pid)
            && let Ok(pid) = pid.trim().parse::<u32>()
        {
            break pid;
        }
        assert!(
            Instant::now() < deadline,
            "the session never started its agent"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let agent = crate::process::identity::capture(agent)
        .unwrap()
        .expect("the agent runs");
    stop_session(session);
    // An orphaned agent may stay a zombie where nothing reaps it, which
    // the identity check counts as ended.
    while crate::process::identity::capture(agent.pid)
        .unwrap()
        .as_ref()
        == Some(&agent)
    {
        assert!(Instant::now() < deadline, "the agent outlived its session");
        std::thread::sleep(Duration::from_millis(20));
    }
}
