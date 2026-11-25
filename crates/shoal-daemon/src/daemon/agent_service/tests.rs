//! Tests of the agents' behaviour across subsystems: what the daemon's
//! spaces, chats, alerts, executions and queue do with the agents' service.
mod control;
mod engagement;
mod execution_connection;
mod executions;
mod idle_wakes;
mod keep_awake;
mod lost_reports;
mod messages;
mod overload;
mod places;
mod proxy_retries;
pub(crate) mod queue;
mod restart_recovery;
mod resumes;
mod sessions;
mod settle;
mod shutdown_stop;
mod stopped_work;
mod turn_retry;
mod turn_state;
mod upstream_limits;
mod wake_timers;
mod warnings;
