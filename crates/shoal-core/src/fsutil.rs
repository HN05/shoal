//! Filesystem operations shared by the CLI and daemon, a module for each
//! responsibility.
pub use crate::client::executions::FileIdentity;
pub use crate::client::fsutil::read_optional;
pub mod atomic;
pub mod copy;
pub mod current_dir;
pub(crate) mod descriptors;
pub mod disk_space;
pub mod executables;
pub mod home;
pub mod marked_dirs;
pub mod off_runtime;
pub mod subvolume;

#[cfg(test)]
pub(crate) mod tests;
