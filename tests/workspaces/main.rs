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
mod creation;
mod demos;
mod executions;
mod fixture;
mod forge;
mod happy;
mod herdr;
mod herdr_default;
mod hooks;
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
    fixture::Base,
    git::{configure_identity, git, git_with_identity, init_repo},
    install_test_script, install_test_script_with_mode, ports, prompt, pty,
    wait::{try_wait_for, try_wait_within, wait_for, wait_until},
};

use fixture::{
    Fixture, PASSTHROUGH_ZMX, READABLE_SLEEP, UNLIMITED_AGENTS, commit_file,
    commit_resource_config, install_fake_agents, lines, link_runner_program, process_running,
    scoped_command, scoped_hook, set_repository_toml, upstream_remote, wait_registered_execution,
};
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
