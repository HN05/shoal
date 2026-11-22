//! What `shoal-runner` needs of the rest of the daemon for its triggers and
//! the CI jobs it runs, which the daemon implements here.
mod agents;
mod executions;
mod forges;
mod job_workspaces;
mod permissions;
mod records;
mod repositories;
mod spaces;

pub(crate) use records::RunnerRecords;
pub(crate) use shoal_daemon_base::jobs::JobStop;

#[cfg(test)]
mod tests;
