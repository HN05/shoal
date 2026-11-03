//! Running repositories' CI jobs on this machine: the runner each
//! repository registers on its forge, the forge's waiting jobs as entries of
//! the agent queue, and the jobs the daemon claims, each in a workspace at its
//! commit that goes when the job ends.
mod registration;
mod waiting;

pub(crate) use registration::Registration;

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
pub(crate) fn runner_labels(configured: &[String]) -> Vec<String> {
    let mut labels = configured.to_vec();
    let platform = platform_label();
    if !labels.contains(&platform) {
        labels.push(platform);
    }
    labels
}
