//! Tests of the agents' behaviour across subsystems: what the daemon's
//! spaces, chats, alerts, executions and queue do with the agents' service.
mod engagement;
mod idle_wakes;
mod keep_awake;
mod lost_reports;
mod messages;
mod settle;
mod turn_retry;
mod turn_state;
mod wake_timers;
