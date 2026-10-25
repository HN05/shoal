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
        /// A server on a reserved port the user reaches by address.
        Server => "server",
        /// A server on a reserved port the user opens in a browser.
        Web => "web",
        /// An app on the workspace's leased simulator.
        Simulator => "simulator",
    }
);

states!(
    /// How a web demo's URL begins.
    Scheme: ValueEnum {
        Http => "http",
        Https => "https",
    }
);

impl Preset {
    /// Whether the demo serves on a reserved port.
    pub fn serves(self) -> bool {
        matches!(self, Self::Server | Self::Web)
    }
}

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
    /// A web demo's URL scheme; `http` when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheme: Option<Scheme>,
}

/// What a demo's wrapper records with its execution: everything opening,
/// reading or typing into the demo needs later.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Launch {
    /// Unique to one start, so the starting CLI finds its own record.
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<Preset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheme: Option<Scheme>,
    /// Variables the preset gives the command, such as `PORT`; opening the
    /// demo receives them too.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
    /// The terminal host the demo runs in, and its name for the demo's place
    /// there, such as a zmx session.
    pub host: String,
    pub place: String,
}

/// A running demo as listings show it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Running {
    pub workspace_id: String,
    pub workspace: String,
    pub execution_id: String,
    /// Unix seconds.
    pub started_at: u64,
    /// The demo's command has started; a start reports the demo only then.
    pub has_run: bool,
    #[serde(flatten)]
    pub launch: Launch,
}

/// How far a launch the starting CLI waits on got.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Progress {
    /// Not recorded yet, or recorded before its command started.
    Starting,
    Running {
        demo: Box<Running>,
    },
    /// Its execution ended before its command started.
    Ended,
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
        self.validate_scheme()
    }

    /// A scheme belongs to a web demo's URL.
    pub fn validate_scheme(&self) -> Result<()> {
        ensure!(
            self.scheme.is_none() || self.preset == Some(Preset::Web),
            "scheme applies only to the web preset"
        );
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
            scheme: None,
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
        let mut https = demo(&["server"]);
        https.scheme = Some(Scheme::Https);
        assert!(https.validate().is_err());
        https.preset = Some(Preset::Web);
        assert!(https.validate().is_ok());
    }
}
