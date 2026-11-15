//! A checkpoint's number among its workspace's checkpoints, which commands
//! select it by.

crate::macros::number!(
    /// The number of one of a workspace's checkpoints, which counts them
    /// from 1.
    CheckpointNumber(u64)
);
