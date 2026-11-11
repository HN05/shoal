use std::process::Command;

use super::*;

#[test]
fn running_processes_are_described_and_exited_ones_left_out() {
    let mut child = Command::new("sleep").args(["300", "1"]).spawn().unwrap();
    let running = child.id();
    let mut exited = Command::new("true").spawn().unwrap();
    let gone = exited.id();
    exited.wait().unwrap();

    let described = describe(&[std::process::id(), running, gone]);
    child.kill().unwrap();
    child.wait().unwrap();
    let described = described.unwrap();

    assert_eq!(described[&running].command, "sleep 300 1");
    assert!(described[&std::process::id()].elapsed >= described[&running].elapsed);
    assert!(!described.contains_key(&gone));
}
