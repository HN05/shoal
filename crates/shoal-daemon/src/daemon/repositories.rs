//! What the repositories' service needs of the rest of the daemon, which
//! the daemon implements here.
mod agents;
mod branches;
mod followers;
mod host;
mod occupants;
mod records;
mod retention;
mod spaces;

pub(crate) use records::RepositoryRecords;

#[cfg(test)]
mod tests;
