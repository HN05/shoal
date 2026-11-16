//! Telling the daemon that a signal ends an execution's session before the
//! wrapper stops its command, when the execution's plan asks for it: the
//! daemon first ends what the command runs elsewhere, such as a CI job's run
//! on its forge, whose runner would otherwise report the stopped job failed.
//! The daemon keeps the forge's login, so a wrapper that cannot reach it
//! stops the command all the same and records why in its log.
use std::time::Duration;

use tokio::time::timeout;

use crate::{
    client::connection::Connector, model::ExecutionPlan, paths::Paths, protocol::SessionSignal,
};

/// How long the wrapper waits for the daemon before it stops the command
/// anyway: a shutdown gives the wrapper little more.
const REPORT_LIMIT: Duration = Duration::from_secs(15);

/// What a wrapper tells the daemon of the signal ending its execution's
/// session.
pub(super) struct SignalReport<'a> {
    pub(super) paths: &'a Paths,
    pub(super) plan: &'a ExecutionPlan,
}

impl SignalReport<'_> {
    /// Tell the daemon that `signal` ends the session, when the plan asks for
    /// it, and wait for its answer.
    pub(super) async fn send(&self, signal: i32) {
        let Some(signal) = session_signal(signal).filter(|_| self.plan.reports_signals) else {
            return;
        };
        let daemon = Connector {
            paths: self.paths.clone(),
            scope: Some(self.plan.scope_token.clone()),
        };
        let reported = timeout(
            REPORT_LIMIT,
            crate::client::executions::report_session_signal(&daemon, signal),
        )
        .await
        .unwrap_or_else(|_| Err(anyhow::anyhow!("the daemon did not answer in time")));
        if let Err(error) = reported {
            eprintln!(
                "shoal: {signal} stops the command before the daemon ended its work elsewhere: {error:#}"
            );
        }
    }
}

/// The session signal `signal` is, if it is one.
fn session_signal(signal: i32) -> Option<SessionSignal> {
    match signal {
        libc::SIGHUP => Some(SessionSignal::Hangup),
        libc::SIGTERM => Some(SessionSignal::Terminate),
        _ => None,
    }
}
