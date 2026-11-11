use std::process::Command;

use super::*;

#[test]
fn running_processes_are_described_and_exited_ones_left_out() {
    let mut child = Command::new("sleep").arg("300").spawn().unwrap();
    let running = child.id();
    let mut exited = Command::new("true").spawn().unwrap();
    let gone = exited.id();
    exited.wait().unwrap();

    let described = describe(&[std::process::id(), running, gone]);
    child.kill().unwrap();
    child.wait().unwrap();
    let described = described.unwrap();

    assert_eq!(described[&running].command, "sleep 300");
    assert!(described[&std::process::id()].elapsed >= described[&running].elapsed);
    assert!(!described.contains_key(&gone));
}

#[test]
fn control_characters_show_as_question_marks() {
    let arguments = ["sh".into(), "-c".into(), "echo a\nb\x1b[2J\t".into()];
    assert_eq!(printable(&command_line(&arguments)), "sh -c echo a?b?[2J?");
}
