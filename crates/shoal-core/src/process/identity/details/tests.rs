use super::*;

#[test]
fn own_command_lines_list_a_child_with_its_arguments() {
    let mut child = std::process::Command::new("sleep")
        .arg("300")
        .spawn()
        .unwrap();
    let listed = own_command_lines();
    child.kill().unwrap();
    child.wait().unwrap();

    let found = listed
        .unwrap()
        .into_iter()
        .find(|process| process.pid == child.id())
        .map(|process| (process.parent, process.arguments));
    assert_eq!(
        found,
        Some((std::process::id(), vec!["sleep".into(), "300".into()]))
    );
}

#[test]
fn a_pid_that_names_no_process_shows_nothing() {
    for pid in [0, u32::MAX] {
        assert_eq!(arguments(pid), None);
        assert_eq!(elapsed(pid).unwrap(), None);
        assert_eq!(terminal(pid).unwrap(), None);
        assert_eq!(foreground_group(pid).unwrap(), None);
    }
}
