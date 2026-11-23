use std::time::Duration;

use tokio::process::Command;

use super::*;

/// Whether a process with ID `pid` exists, a zombie included.
fn exists(pid: libc::pid_t) -> bool {
    // SAFETY: kill takes no pointers, and signal 0 sends nothing.
    unsafe { libc::kill(pid, 0) == 0 }
}

#[tokio::test]
async fn a_detached_execution_s_wrapper_is_reaped_once_it_exits() -> Result<()> {
    let wrapper = Command::new("true").spawn()?;
    let pid = libc::pid_t::try_from(wrapper.id().context("the wrapper has an ID")?)?;
    let execution = DetachedExecution {
        launch: DetachedLaunch {
            execution_id: ExecutionId::from("execution".to_owned()),
            pid: 0,
            log: PathBuf::new(),
        },
        wrapper,
    };
    execution.detach();
    // An unreaped wrapper stays a zombie, which signal 0 still finds; this
    // only hangs, until the guard, while it is never reaped.
    timeout(Duration::from_secs(60), async {
        while exists(pid) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .context("the wrapper was never reaped")?;
    Ok(())
}
