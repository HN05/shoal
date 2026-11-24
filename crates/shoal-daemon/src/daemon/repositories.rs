//! What the daemon gives the repositories' service: the spaces whose
//! settings it reads and that a removal takes, those who follow a local
//! config, the subsystems that work and
//! keep files in a repository's directories, the agents that trust its
//! paths, the refresh of a checkout's default branch, the host's headroom,
//! the forges a contribution forks on, and the records of other
//! subsystems it reads and writes.
mod agents;
mod branches;
mod followers;
mod host;
mod occupants;
mod records;
mod spaces;

pub(crate) use records::RepositoryRecords;

#[cfg(test)]
mod tests;
