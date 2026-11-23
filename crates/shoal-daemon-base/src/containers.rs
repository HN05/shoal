//! What a workspace's containers use, as their runtime reports it.

/// What a project's running containers use together.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct ContainerUsage {
    /// Percent of one core.
    pub cpu_percent: f64,
    pub memory_bytes: u64,
}
