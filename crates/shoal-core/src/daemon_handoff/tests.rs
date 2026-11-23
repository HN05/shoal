use super::*;

#[test]
fn an_inherited_child_is_reaped_once_it_exits() -> Result<()> {
    // Dropping a std child leaves it unreaped, as the image an exec replaced
    // left its children.
    let pid = Command::new("true").spawn()?.id();
    reap(pid);
    let mut status = 0;
    // SAFETY: waitpid writes only the status, which outlives the call.
    let waited = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
    assert_eq!(
        (waited, std::io::Error::last_os_error().raw_os_error()),
        (-1, Some(libc::ECHILD)),
        "the child was not reaped"
    );
    Ok(())
}
