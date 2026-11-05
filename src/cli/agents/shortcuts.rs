//! `shoal happy`, which starts a Happy session in a workspace, and whether
//! it can.
use std::ffi::{OsStr, OsString};

use anyhow::Result;

#[cfg(feature = "happy")]
use crate::happy::Agent;
use crate::{cli::context::Context, config::named_commands::Commands};

/// `shoal happy <agent>`.
#[cfg(feature = "happy")]
pub(in crate::cli) async fn happy(
    ctx: &Context,
    agent: Agent,
    workspace: Option<String>,
    prompt: Option<String>,
    args: Vec<OsString>,
) -> Result<i32> {
    let name = format!("happy-{}", agent.as_str()).parse()?;
    let options = super::LaunchOptions::default();
    let subject = prompt.map(crate::config::templates::Subject::told);
    super::launch::shortcut(ctx, name, workspace, subject, args, options).await
}

/// The agents `shoal happy <agent>` can start: each `happy-<agent>` offered
/// and installed.
#[cfg(feature = "happy")]
pub(in crate::cli) fn happy_agents<'a>(installed: &Installed<'a>) -> Vec<&'a str> {
    installed
        .agents
        .iter()
        .filter(|name| installed.agent(name))
        .filter_map(|name| name.strip_prefix("happy-"))
        .collect()
}

/// The agents offered under some settings, with their commands and the
/// search path, from which shortcuts judge whether they can start anything.
pub(in crate::cli) struct Installed<'a> {
    #[cfg_attr(not(feature = "happy"), allow(dead_code))]
    pub(in crate::cli) agents: &'a [String],
    pub(in crate::cli) commands: &'a Commands,
    pub(in crate::cli) search_path: &'a OsStr,
}

impl Installed<'_> {
    /// Whether agent `name` can launch.
    fn agent(&self, name: &str) -> bool {
        super::installed(name, self.commands, self.search_path)
    }
}

/// Each shortcut subcommand with whether it can start anything, so
/// completion offers only those that can.
pub(in crate::cli) fn shortcuts(installed: &Installed) -> Vec<(&'static str, bool)> {
    vec![("happy", !happy_agents(installed).is_empty())]
}

/// The agents `shoal happy <agent>` can start.
#[cfg(not(feature = "happy"))]
pub(in crate::cli) fn happy_agents<'a>(_installed: &Installed<'a>) -> Vec<&'a str> {
    Vec::new()
}
