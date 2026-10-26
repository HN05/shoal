use super::*;

#[test]
fn a_binary_installed_after_the_start_replaces_the_missing_one() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("shoal");
    let executable = Executable::at(path.clone());
    assert!(!executable.replaced(), "a missing binary cannot take over");
    std::fs::write(&path, "new").unwrap();
    assert!(executable.replaced());
}

// A failed exec would leave this process without its ignored SIGPIPE.
#[test]
fn only_a_protocol_change_on_a_replaced_binary_hands_the_command_over() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("shoal");
    std::fs::write(&path, "old").unwrap();
    let executable = Executable::at(path.clone());
    let mismatch = executable.resume_or(ProtocolMismatch { daemon: 0 }.into(), || {
        unreachable!("the binary was not replaced")
    });
    assert!(mismatch.is::<ProtocolMismatch>());
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, "new").unwrap();
    let error = executable.resume_or(anyhow::anyhow!("lookup failed"), || {
        unreachable!("only a protocol change hands the command over")
    });
    assert_eq!(error.to_string(), "lookup failed");
}

#[test]
fn a_replaced_option_keeps_the_other_arguments() {
    let arguments =
        |arguments: &[&str]| replace_option(arguments.iter().map(OsString::from), "--since", "9");
    assert_eq!(
        arguments(&["internal", "events", "--since", "3", "--follow"]),
        ["internal", "events", "--follow", "--since", "9"]
    );
    assert_eq!(
        arguments(&["--json", "internal", "events", "--since=3"]),
        ["--json", "internal", "events", "--since", "9"]
    );
    assert_eq!(
        arguments(&["internal", "events", "--follow"]),
        ["internal", "events", "--follow", "--since", "9"]
    );
}

#[test]
fn a_flag_is_added_once() {
    let arguments =
        |arguments: &[&str]| with_flag(arguments.iter().map(OsString::from).collect(), "--resume");
    assert_eq!(arguments(&["watch"]), ["watch", "--resume"]);
    assert_eq!(arguments(&["watch", "--resume"]), ["watch", "--resume"]);
}
