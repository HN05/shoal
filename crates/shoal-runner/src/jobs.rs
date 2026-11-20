//! The CI jobs this machine runs for repositories' forges: what stops a
//! job's runner, and the labels a runner registers with.
use shoal_protocol::protocol::SessionSignal;

/// What stopped a job's runner, as Shoal's log names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobStop {
    /// `shoal stop` of its workspace.
    Stop,
    /// `shoal daemon stop`.
    DaemonStop,
    /// A signal to the daemon.
    DaemonSignal,
    /// The stop before a system shutdown.
    SystemShutdown,
    /// The removal of its workspace.
    Removal,
    /// Its runner outliving its finished run.
    Overrun,
    /// A signal ending its wrapper's session, as a logout or a shutdown
    /// sends.
    Signal(SessionSignal),
}

impl std::fmt::Display for JobStop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stop => f.write_str("shoal stop"),
            Self::DaemonStop => f.write_str("shoal daemon stop"),
            Self::DaemonSignal => f.write_str("a signal to the daemon"),
            Self::SystemShutdown => f.write_str("the system shutting down"),
            Self::Removal => f.write_str("its workspace's removal"),
            Self::Overrun => f.write_str("its runner outliving its finished run"),
            Self::Signal(signal) => write!(f, "{signal} to its wrapper"),
        }
    }
}

/// The label every runner registers besides the configured ones, naming the
/// machine's system and architecture, such as `shoal-macos-arm64`.
fn platform_label() -> String {
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        arch => arch,
    };
    format!("shoal-{}-{arch}", std::env::consts::OS)
}

/// `configured` with the platform label, each once.
pub fn runner_labels(configured: &[String]) -> Vec<String> {
    let mut labels = configured.to_vec();
    let platform = platform_label();
    if !labels.contains(&platform) {
        labels.push(platform);
    }
    labels
}
