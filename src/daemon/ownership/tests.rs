use super::*;

#[test]
fn the_lock_names_the_daemon_that_holds_it() -> Result<()> {
    let root = tempfile::tempdir_in("/tmp")?;
    let paths = Paths::for_test(root.path());
    paths.prepare()?;
    let ownership = Ownership::acquire(&paths, false, None)?;
    let owner = owner(&paths).context("no owner recorded")?;
    assert_eq!(owner.pid, std::process::id());
    drop(ownership);
    Ok(())
}

#[test]
fn a_lock_naming_an_exited_process_names_no_owner() -> Result<()> {
    let root = tempfile::tempdir_in("/tmp")?;
    let paths = Paths::for_test(root.path());
    paths.prepare()?;
    let mut child = std::process::Command::new("/usr/bin/true").spawn()?;
    let exited = identity::capture(child.id())?;
    child.wait()?;
    let Some(exited) = exited else {
        // The child ended before its identity was read; nothing to record.
        return Ok(());
    };
    fs::write(paths.daemon_lock(), serde_json::to_vec(&exited)?)?;
    assert!(owner(&paths).is_none());
    Ok(())
}
