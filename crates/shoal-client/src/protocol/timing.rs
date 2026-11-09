//! Shared CLI, daemon, and wrapper timing; keep related budgets in view when tuning.
use std::time::Duration;

/// Release a daemon connection slot if the client never sends its request.
pub const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(5);
/// Status and shutdown should fail promptly when the daemon is unresponsive.
pub const ADMIN_REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
/// A status request during daemon startup waits this long for it to end
/// before learning the running step, within the admin request timeout.
pub const STARTUP_STATUS_WAIT: Duration = Duration::from_secs(2);
/// Ordinary requests may include slow Git, simulator, or hook work.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(3600);
/// Allow daemon startup or shutdown to settle across repeated status requests.
pub const DAEMON_WAIT_TIMEOUT: Duration = Duration::from_secs(10);
/// A long-lived wait that lost its daemon gives the next one this long to
/// listen and know the caller's scope again: the service manager's restart
/// throttle, the startup audit, and the wrapper's reattachment.
pub const DAEMON_RESTART_TIMEOUT: Duration = Duration::from_secs(120);
/// Observe daemon transitions promptly without busy-polling the socket.
pub const DAEMON_POLL_INTERVAL: Duration = Duration::from_millis(100);
/// A managed daemon restarts with an installed update once its running
/// operations finish, which can include a long setup.
pub const UPDATE_WAIT_TIMEOUT: Duration = Duration::from_secs(600);
/// Each refused attempt briefly holds back the update it waits for.
pub const UPDATE_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Ordinary command registration needs no slow preparation, but its process
/// and worktree checks run subprocesses; only guard against a hung daemon.
pub const EXECUTION_START_TIMEOUT: Duration = Duration::from_secs(30);
/// Setup and merge can wait for Git gates, hooks, and upstream refreshes.
pub const PREPARED_EXECUTION_START_TIMEOUT: Duration = Duration::from_secs(120);
/// Wait for the daemon to persist the spawned child's process group.
pub const START_ACK_TIMEOUT: Duration = Duration::from_secs(10);
/// Cover stopping an exited agent's survivors and the completion scan,
/// with room for ownership checks and persistence.
pub const COMPLETION_ACK_TIMEOUT: Duration = Duration::from_secs(60);
/// Bound the commands that inspect processes: the `ps` that describes
/// them and the probe that finds a Linux subreaper. The completion budget
/// allows it for each process inventory too.
pub const PROCESS_INVENTORY_TIMEOUT: Duration = Duration::from_secs(10);
/// Give an identity-verified survivor time to handle SIGTERM before SIGKILL.
pub const PROCESS_TERM_GRACE: Duration = Duration::from_secs(2);
/// Wait for a survivor to disappear after SIGKILL before reporting it.
pub const PROCESS_KILL_WAIT: Duration = Duration::from_secs(1);
/// Wait within one scan for processes caught mid-exec to publish their
/// environment, and again for the ones an inspection finds unattributed.
pub const PROCESS_SETTLE_TIMEOUT: Duration = Duration::from_secs(2);
/// Cover ordinary EXECUTION_START_TIMEOUT plus START_ACK_TIMEOUT and wrapper startup.
pub const DETACHED_LAUNCH_TIMEOUT: Duration = Duration::from_secs(60);
/// Collect a failed detached wrapper's exit status without waiting indefinitely.
pub const DETACHED_EXIT_TIMEOUT: Duration = Duration::from_secs(5);
/// Give the child time to handle a stop signal before escalating to SIGKILL.
pub const EXECUTION_STOP_GRACE: Duration = Duration::from_secs(2);
/// Retry incomplete process proof without extending the wrapper completion budget.
pub const PROCESS_PROOF_TIMEOUT: Duration = Duration::from_secs(10);
/// Cover child registration, its stop grace, and the daemon completion acknowledgement.
pub const WORKSPACE_STOP_TIMEOUT: Duration = Duration::from_secs(90);
/// Notice completed execution records promptly while workspace stopping waits.
pub const WORKSPACE_STOP_POLL_INTERVAL: Duration = Duration::from_millis(50);
/// First retry of a detached wrapper; the delay doubles up to the cap.
pub const REATTACH_RETRY_INITIAL: Duration = Duration::from_millis(100);
/// Keep a detached wrapper's scope gap short once the daemon returns.
pub const REATTACH_RETRY_MAX: Duration = Duration::from_secs(1);
/// Give up on an unresponsive daemon's reattach reply and retry.
pub const REATTACH_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
/// After its command exits, wait this long for a restarting daemon to take
/// the exit report; cover a stop with its workspace drain plus a start.
pub const REATTACH_EXIT_TIMEOUT: Duration = Duration::from_secs(120);

const _: () = {
    // Each pass scans markers, then inspects related processes with a
    // second inventory and lets unattributed ones settle; an exited agent
    // gets a stop pass before completion.
    assert!(
        COMPLETION_ACK_TIMEOUT.as_millis()
            > 2 * (2 * PROCESS_INVENTORY_TIMEOUT.as_millis()
                + 2 * PROCESS_SETTLE_TIMEOUT.as_millis())
                + PROCESS_TERM_GRACE.as_millis()
                + PROCESS_KILL_WAIT.as_millis()
    );
    assert!(
        DETACHED_LAUNCH_TIMEOUT.as_millis()
            > EXECUTION_START_TIMEOUT.as_millis() + START_ACK_TIMEOUT.as_millis()
    );
    assert!(
        WORKSPACE_STOP_TIMEOUT.as_millis()
            > START_ACK_TIMEOUT.as_millis()
                + EXECUTION_STOP_GRACE.as_millis()
                + COMPLETION_ACK_TIMEOUT.as_millis()
    );
    assert!(PROCESS_PROOF_TIMEOUT.as_millis() < COMPLETION_ACK_TIMEOUT.as_millis());
    assert!(
        REATTACH_EXIT_TIMEOUT.as_millis()
            > WORKSPACE_STOP_TIMEOUT.as_millis() + 2 * DAEMON_WAIT_TIMEOUT.as_millis()
    );
    assert!(
        DAEMON_RESTART_TIMEOUT.as_millis()
            > DAEMON_WAIT_TIMEOUT.as_millis() + REATTACH_RETRY_MAX.as_millis()
    );
};
