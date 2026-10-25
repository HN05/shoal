//! Demos: named commands Shoal keeps running in a workspace so the user can
//! try a change, usually from another machine over SSH. A repository names
//! them in `[demos.NAME]`; a preset says how a demo reaches the user, and the
//! repository always supplies the command.
use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::state::states;

states!(
    /// How a demo reaches the user, beyond attaching to its session.
    Preset: ValueEnum {
        /// A server on a reserved port the user opens by URL.
        Web => "web",
        /// An app on the workspace's leased simulator.
        Simulator => "simulator",
    }
);

/// One `[demos.NAME]` table.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Demo {
    /// The command, run in the workspace and restarted when it exits.
    pub cmd: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<Preset>,
    /// What `shoal demo NAME` runs in the user's terminal instead of the
    /// preset's way of opening the demo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open: Option<Vec<String>>,
    /// Port reservations made before the demo starts, replacing the preset's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ports: Option<Vec<String>>,
}

/// What a demo's wrapper records with its execution: everything opening,
/// reading or typing into the demo needs later.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Launch {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<Preset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open: Option<Vec<String>>,
    /// Variables the preset gives the command, such as `PORT`; opening the
    /// demo receives them too.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
    /// The terminal host the demo runs in, and its name for the demo's place
    /// there, such as a zmx session.
    pub host: String,
    pub place: String,
    /// Starting the demo leased the workspace's simulator, which the last
    /// simulator demo to stop releases.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub leased_simulator: bool,
}

/// A running demo as listings show it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Running {
    pub workspace_id: String,
    pub workspace: String,
    pub execution_id: String,
    /// Unix seconds.
    pub started_at: u64,
    #[serde(flatten)]
    pub launch: Launch,
}

/// The `shoal demo` subcommands, which `shoal demo NAME` could not tell
/// apart from a demo of that name.
const RESERVED: &[&str] = &["ls", "screen", "send", "start", "stop"];

pub fn validate(demos: &BTreeMap<String, Demo>) -> Result<()> {
    for (name, demo) in demos {
        validate_name(name).with_context(|| format!("invalid configured demo name: {name}"))?;
        demo.validate()
            .with_context(|| format!("invalid configured demo {name}"))?;
    }
    Ok(())
}

pub fn validate_name(name: &str) -> Result<()> {
    crate::validate::lowercase_name("demo", name)?;
    ensure!(
        !RESERVED.contains(&name),
        "{name} is a shoal demo subcommand"
    );
    Ok(())
}

impl Demo {
    pub fn validate(&self) -> Result<()> {
        argv("cmd", &self.cmd)?;
        if let Some(open) = &self.open {
            argv("open", open)?;
        }
        for port in self.ports.iter().flatten() {
            crate::validate::lowercase_name("port", port)?;
        }
        Ok(())
    }
}

fn argv(key: &str, argv: &[String]) -> Result<()> {
    ensure!(
        argv.first()
            .is_some_and(|program| !program.trim().is_empty())
            && argv.iter().all(|arg| !arg.contains('\0')),
        "{key} needs a nonempty executable and arguments without NUL bytes"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demo(cmd: &[&str]) -> Demo {
        Demo {
            cmd: cmd.iter().map(|arg| (*arg).to_owned()).collect(),
            preset: None,
            open: None,
            ports: None,
        }
    }

    #[test]
    fn names_follow_port_names_and_leave_subcommands_alone() {
        assert!(validate_name("web").is_ok());
        assert!(validate_name("Web").is_err());
        assert!(validate_name("ls").is_err());
        assert!(validate_name("start").is_err());
    }

    #[test]
    fn commands_need_an_executable() {
        assert!(demo(&["npm", "run", "dev"]).validate().is_ok());
        assert!(demo(&[]).validate().is_err());
        assert!(demo(&[" "]).validate().is_err());
        let mut open = demo(&["server"]);
        open.open = Some(Vec::new());
        assert!(open.validate().is_err());
        let mut ports = demo(&["server"]);
        ports.ports = Some(vec!["Web".into()]);
        assert!(ports.validate().is_err());
    }
}
