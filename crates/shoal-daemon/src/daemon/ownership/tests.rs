use super::*;

#[test]
fn the_lock_names_the_daemon_that_holds_it() -> Result<()> {
    let root = tempfile::tempdir_in("/tmp")?;
    let paths = Paths::for_test(root.path());
    paths.prepare()?;
    let ownership = Ownership::acquire(&paths, false, None)?;
    let owner = daemon_owner::owner(&paths).context("no owner recorded")?;
    assert_eq!(owner.pid, std::process::id());
    drop(ownership);
    Ok(())
}
