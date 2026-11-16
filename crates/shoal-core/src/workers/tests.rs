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

/// The program and arguments `command` runs.
fn argv(command: &tokio::process::Command) -> Vec<&std::ffi::OsStr> {
    let command = command.as_std();
    std::iter::once(command.get_program())
        .chain(command.get_args())
        .collect()
}

#[test]
fn detached_command_starts_the_worker_directly_until_a_launcher_is_chosen() {
    let command = detached_command(&["sleep".into(), "60".into()]);
    assert_eq!(argv(&command), ["sleep", "60"]);
}

#[test]
fn detached_command_starts_the_worker_through_the_chosen_launcher() {
    // Nextest runs each test in a process of its own, so no other test
    // starts through this launcher.
    crate::service::apart::remember(vec!["systemd-run".into(), "--scope".into(), "--".into()]);
    let command = detached_command(&["sleep".into(), "60".into()]);
    assert_eq!(
        argv(&command),
        ["systemd-run", "--scope", "--", "sleep", "60"]
    );
}
