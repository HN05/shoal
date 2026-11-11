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
