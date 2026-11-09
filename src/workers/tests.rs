use super::*;
use anyhow::Context as _;

#[test]
fn shoal_argv_names_even_the_default_state_directory() {
    let default = Paths {
        state: "/home/.local/state/shoal".into(),
        ..Paths::for_test("/home")
    };
    assert_eq!(
        shoal_argv(&default).unwrap()[1..],
        ["--state-dir", "/home/.local/state/shoal", "--text"]
    );
}

#[tokio::test]
async fn detached_command_starts_the_worker_in_a_session_of_its_own() -> anyhow::Result<()> {
    let mut child = detached_command(&["sleep".into(), "60".into()]).spawn()?;
    let pid = child.id().context("the worker has an ID")? as libc::pid_t;
    // SAFETY: getsid takes no pointers.
    let session = unsafe { libc::getsid(pid) };
    child.kill().await?;
    assert_eq!(session, pid);
    Ok(())
}
