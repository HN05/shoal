// Keep the workspace suite in one integration target so it shares one harness.
#[path = "../common/mod.rs"]
mod common;
#[path = "../support/workspaces.rs"]
mod support;

mod agents;
mod approvals;
mod branches;
mod cleanup;
mod completion;
mod config;
mod execution;
mod git;
mod happy;
mod hooks;
mod issues;
mod navigation;
mod notifications;
mod permits;
mod ports;
mod pr_cleanup;
mod recovery;
mod repositories;
mod review;
mod setup;
#[cfg(target_os = "macos")]
mod simulators;
mod status;
