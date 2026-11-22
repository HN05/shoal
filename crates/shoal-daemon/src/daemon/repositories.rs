//! What the daemon gives the repositories' service: the spaces whose
//! settings it reads and that a removal takes, the subsystems that work and
//! keep files in a repository's directories, the agents that trust its
//! paths, the forges a contribution forks on, and the records of other
//! subsystems it reads and writes.
mod agents;
mod forges;
mod occupants;
mod records;
mod spaces;

pub(crate) use records::RepositoryRecords;

#[cfg(test)]
mod tests;
