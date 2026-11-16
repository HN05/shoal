//! Timing lines for profiling Shoal itself, which a `shoal` process also
//! prints for each subprocess it runs.
pub use shoal_protocol::profile::*;

/// Time every subprocess run from now on, when profiling.
pub fn start() {
    if enabled() {
        crate::subprocess::observe(|command, elapsed| line("run", elapsed, command));
    }
}
