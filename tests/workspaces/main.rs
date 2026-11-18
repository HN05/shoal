//! Workspace integration tests, one module per area, each against the
//! daemon its `Fixture` starts.
mod adoption;
mod approvals;
mod branches;
mod chat_stops;
mod chats;
mod commands;
mod completion;
mod configuration;
mod containers;
mod creation;
mod demos;
mod executions;
mod fixture;
mod forge;
mod happy;
mod herdr;
mod herdr_default;
mod hooks;
mod launch;
mod master;
mod names;
mod notifications;
mod recovery;
mod removal;
mod repositories;
mod resources;
mod restarts;
mod rex;
mod secrets;
#[cfg(target_os = "macos")]
mod simulators;
mod start;
mod stopped_work;
mod stopping;
mod terminal_sessions;
mod tmux;
mod transfer;
mod ui;
mod upgrades;
mod zellij;
mod zmx;

#[path = "../support/mod.rs"]
mod support;

use support::{
    cli,
    daemon::DaemonGuard,
    fake_agent_script,
    fixture::Base,
    git, git_with_identity, install_fake_agent, install_test_script, install_test_script_with_mode,
    ports, printed, prompt, pty, repository_with,
    wait::{try_wait_for, try_wait_within, wait_for, wait_until},
    zmx::{PASSTHROUGH_ZMX, terminal_zmx},
};

use fixture::{
    Fixture, READABLE_SLEEP, SLEEPING_AGENT, UNLIMITED_AGENTS, commit_file, commit_resource_config,
    executions, install_fake_agents, lines, link_runner_program, process_running, scoped_command,
    scoped_hook, sessions, set_repository_toml, upstream_remote, wait_for_agent,
    wait_registered_execution,
};
use launch::Launch;
use serde_json::Value;
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::Mutex,
    thread,
    time::{Duration, Instant},
};
use tempfile::TempDir;
