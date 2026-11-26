//! Demos, the commands Shoal keeps running in a workspace for the user to
//! try a change. See `docs/design/demos.md`.
use crate::space_names::SpaceName;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use shoal_states::states;

/// The variable a serving demo's port reaches its command as.
pub const PORT: &str = "PORT";
/// The variable a simulator demo's device reaches its command as.
pub const SIM_UDID: &str = "SHOAL_SIM_UDID";
/// What shows a simulator demo's device unless its `viewer` says otherwise:
/// serve-sim's preview, which streams the device over HTTP and takes input
/// over a WebSocket on the one port.
pub const DEFAULT_VIEWER: &[&str] = &["serve-sim", "--port", "{port}", "{udid}"];

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

states!(
    /// What a running demo shows, which says how a client shows it.
    Kind {
        /// A terminal: the screen of the place it runs in.
        Terminal => "terminal",
        /// A server on a port, which a client reaches by TCP.
        Server => "server",
        /// A server on a port, which a client opens as a web page.
        Web => "web",
        /// An app on a simulator.
        Simulator => "simulator",
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
    /// What shows a simulator demo's device on another machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewer: Option<ViewerSetting>,
}

/// A simulator demo's `viewer`: a command, with `{udid}` and `{port}`
/// filled in, or whether [`DEFAULT_VIEWER`] runs.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ViewerSetting {
    Enabled(bool),
    Command(Vec<String>),
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
    /// The command that shows a simulator demo's device, before `{udid}`
    /// and `{port}` are filled in; none when its viewer is off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewer_command: Option<Vec<String>>,
    /// Variables the preset gives the command, such as `PORT`; opening the
    /// demo receives them too.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
    /// The place the demo runs in, such as a zmx session.
    #[serde(flatten)]
    pub origin: crate::sessions::Origin,
}

/// A running demo as listings show it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Running {
    pub workspace_id: SpaceId,
    pub workspace: SpaceName,
    pub execution_id: ExecutionId,
    pub started_at: crate::time::UnixTime,
    /// The demo's command has started; a start reports the demo only then.
    pub has_run: bool,
    pub kind: Kind,
    /// Where a browser opens a web demo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// What shows a simulator demo's device, unless its viewer is off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewer: Option<Viewer>,
    #[serde(flatten)]
    pub launch: Launch,
    /// The other joined machine that runs it and answered for it, from
    /// which a demo opens here through the hub.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<crate::hub::Machine>,
}

/// A simulator demo's viewer as listings show it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Viewer {
    /// The program it runs, such as `serve-sim`, which says what protocol
    /// a client speaks to it.
    pub tool: String,
    /// The port it serves on, on the demo's machine, while it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Where a browser opens it there, while it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

impl Launch {
    /// What the demo shows: its preset's, else a terminal.
    pub fn kind(&self) -> Kind {
        match self.preset {
            None => Kind::Terminal,
            Some(Preset::Server) => Kind::Server,
            Some(Preset::Web) => Kind::Web,
            Some(Preset::Simulator) => Kind::Simulator,
        }
    }

    /// The port a serving demo listens on, on this machine.
    pub fn port(&self) -> Option<u16> {
        self.environment.get(PORT)?.parse().ok()
    }

    /// Where a serving demo listens, on this machine.
    pub fn address(&self) -> Option<String> {
        let port = self.environment.get(PORT)?;
        Some(format!("localhost:{port}"))
    }

    /// Where a browser opens a web demo.
    pub fn url(&self) -> Option<String> {
        if self.preset != Some(Preset::Web) {
            return None;
        }
        let scheme = self.scheme.unwrap_or(Scheme::Http);
        Some(format!("{scheme}://{}", self.address()?))
    }

    /// Where the user reaches a serving demo: a web demo's URL, otherwise
    /// its address.
    pub fn location(&self) -> Option<String> {
        self.url().or_else(|| self.address())
    }

    /// The demo's viewer, which serves at `port` while it runs.
    pub fn viewer(&self, port: Option<u16>) -> Option<Viewer> {
        let program = self.viewer_command.as_ref()?.first()?;
        let tool = std::path::Path::new(program).file_name().map_or_else(
            || program.clone(),
            |name| name.to_string_lossy().into_owned(),
        );
        let url = port.map(|port| format!("http://localhost:{port}"));
        Some(Viewer { tool, port, url })
    }

    /// The viewer's command for the device and `port`: `{udid}` and
    /// `{port}` filled in wherever they appear.
    pub fn filled_viewer_command(&self, port: u16) -> Option<Vec<String>> {
        let udid = self.environment.get(SIM_UDID).map_or("", String::as_str);
        let port = port.to_string();
        let fill = |arg: &String| arg.replace("{udid}", udid).replace("{port}", &port);
        Some(self.viewer_command.as_ref()?.iter().map(fill).collect())
    }
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
        if let Some(ViewerSetting::Command(viewer)) = &self.viewer {
            argv("viewer", viewer)?;
        }
        self.validate_scheme()?;
        self.validate_viewer()
    }

    /// A scheme belongs to a web demo's URL.
    pub fn validate_scheme(&self) -> Result<()> {
        ensure!(
            self.scheme.is_none() || self.preset == Some(Preset::Web),
            "scheme applies only to the web preset"
        );
        Ok(())
    }

    /// A viewer belongs to a simulator demo's device.
    pub fn validate_viewer(&self) -> Result<()> {
        ensure!(
            self.viewer.is_none() || self.preset == Some(Preset::Simulator),
            "viewer applies only to the simulator preset"
        );
        Ok(())
    }

    /// The command that shows the demo's device, before `{udid}` and
    /// `{port}` are filled in: none but for a simulator demo whose viewer is
    /// on.
    pub fn viewer_command(&self) -> Option<Vec<String>> {
        if self.preset != Some(Preset::Simulator) {
            return None;
        }
        match &self.viewer {
            None | Some(ViewerSetting::Enabled(true)) => {
                Some(DEFAULT_VIEWER.iter().map(|arg| (*arg).to_owned()).collect())
            }
            Some(ViewerSetting::Enabled(false)) => None,
            Some(ViewerSetting::Command(command)) => Some(command.clone()),
        }
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
mod tests;
