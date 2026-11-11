use super::*;

#[test]
fn a_lock_naming_an_exited_process_names_no_owner() -> Result<()> {
    let root = tempfile::tempdir_in("/tmp")?;
    let paths = Paths::for_test(root.path());
    paths.prepare()?;
    // `cat` lives until its stdin closes, after its identity is read.
    let mut child = std::process::Command::new("cat")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()?;
    let captured = identity::capture(child.id());
    drop(child.stdin.take());
    child.wait()?;
    let exited = captured?.context("identify the living child")?;
    fs::write(paths.daemon_lock(), serde_json::to_vec(&exited)?)?;
    assert!(owner(&paths).is_none());
    Ok(())
}
