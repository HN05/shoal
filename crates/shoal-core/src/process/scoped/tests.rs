use super::*;

#[test]
fn a_recorded_wrapper_scopes_its_descendants_until_dropped() -> Result<()> {
    let state = tempfile::tempdir()?;
    let records = Records::wrapper(state.path())?;
    assert!(any_recorded(state.path(), [std::process::id()].into_iter()));
    drop(records);
    assert!(!any_recorded(
        state.path(),
        [std::process::id()].into_iter()
    ));
    Ok(())
}

#[test]
fn a_record_of_an_ended_process_does_not_match_its_reused_pid() -> Result<()> {
    let state = tempfile::tempdir()?;
    let directory = state.path().join(DIRECTORY);
    std::fs::create_dir_all(&directory)?;
    let mut ended = identity::capture(std::process::id())?.context("identify this process")?;
    ended.birth.push('0');
    write(&directory, &ended)?;
    assert!(!any_recorded(
        state.path(),
        [std::process::id()].into_iter()
    ));
    Ok(())
}

#[test]
fn a_living_command_keeps_its_record_after_its_wrapper_drops() -> Result<()> {
    let state = tempfile::tempdir()?;
    // `cat` lives until its stdin closes, after the assertion.
    let mut command = std::process::Command::new("cat")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()?;
    let mut records = Records::wrapper(state.path())?;
    records.command(command.id())?;
    drop(records);
    let kept = any_recorded(state.path(), [command.id()].into_iter());
    drop(command.stdin.take());
    command.wait()?;
    assert!(kept);
    Ok(())
}
