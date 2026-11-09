//! `shoal demo start`: choosing the demo, replacing a running one of its
//! name, placing its worker in a terminal host and waiting until it records
//! that it runs.
use std::{collections::BTreeMap, ffi::OsString, time::Duration};

use anyhow::{Context as _, Result, bail, ensure};

use super::{lookup::running, presets, screen::last_lines, stop::stop_named};
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        context::Context,
        output::Style,
        terminal,
        ui::{self, Fallback},
    },
    demos::{self, Demo, Launch, Preset, Progress, Running},
    model::Workspace,
    protocol::ConfigTarget,
    sessions::Origin,
    workers::{Worker, internal_command},
};

/// The name an ad hoc demo takes when the agent gives none.
const DEFAULT_NAME: &str = "demo";
/// How often a start checks that a replaced demo ended and its worker
/// recorded the new one.
const START_POLL: Duration = Duration::from_millis(100);
/// A hang guard for a demo that never ends or a worker that never records it.
const START_LIMIT: Duration = Duration::from_secs(120);

/// What `shoal demo start` replaces in the configured demo.
pub(super) struct Overrides {
    pub preset: Option<Preset>,
    pub scheme: Option<crate::demos::Scheme>,
    pub open: Option<String>,
    pub ports: Vec<String>,
}

impl Overrides {
    fn validate(&self) -> Result<()> {
        for port in &self.ports {
            crate::validate::lowercase_name("port", port)?;
        }
        ensure!(
            self.open
                .as_ref()
                .is_none_or(|open| !open.trim().is_empty()),
            "--open needs a command"
        );
        ensure!(
            self.scheme.is_none() || self.preset.is_none_or(|preset| preset == Preset::Web),
            "--scheme applies only to the web preset"
        );
        Ok(())
    }
}

/// A demo ready to start: its name, definition and command.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Chosen {
    pub name: String,
    pub demo: Demo,
    pub command: Vec<OsString>,
}

pub(super) async fn start(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
    name: Option<String>,
    overrides: Overrides,
    queue: bool,
    command: Vec<OsString>,
) -> Result<i32> {
    // Before a picker opens or a running demo is replaced.
    overrides.validate()?;
    let id = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let workspace = crate::client::workspaces::inspect(&ctx.paths, id.clone())
        .await?
        .workspace;
    let settings = crate::client::workspaces::settings(
        &ctx.paths,
        ConfigTarget::Workspace(SpaceSelector::id(&workspace.id)),
    )
    .await?;
    let chosen = choose(&settings.demos, name, command)?.with(overrides);
    chosen.demo.validate_scheme()?;
    replace(ctx, &workspace, &chosen.name).await?;
    let provided = match presets::provide(ctx, &workspace, &chosen, queue).await? {
        Ok(provided) => provided,
        Err(declined) => return Ok(declined),
    };
    let launch = place(ctx, &settings, &workspace, &chosen, provided).await?;
    let running = await_record(ctx, &workspace, &launch).await?;
    let opening = format!("shoal demo {} --workspace {}", chosen.name, workspace.name);
    if crate::env::is_scoped() {
        let message = format!("Demo {} is running; open it with {opening}", chosen.name);
        let space = workspace.id.clone();
        crate::client::notifications::send(
            &ctx.paths,
            SpaceSelector::id(&space),
            message,
            Vec::new(),
            None,
        )
        .await?;
    }
    ctx.emit_styled(
        Style::Success,
        &format!("Started demo {}; open it with {opening}", chosen.name),
        &running,
    )?;
    Ok(0)
}

/// Stop a running demo `name` and wait for it to end, so the new one neither
/// shares its place nor is mistaken for it.
pub(super) async fn replace(ctx: &Context, workspace: &Workspace, name: &str) -> Result<()> {
    stop_named(ctx, workspace, Some(name.to_owned())).await?;
    let deadline = tokio::time::Instant::now() + START_LIMIT;
    while running(ctx, Some(workspace))
        .await?
        .iter()
        .any(|demo| demo.launch.name == name)
    {
        ensure!(
            tokio::time::Instant::now() < deadline,
            "demo {name} is still stopping; start it again once `shoal demo ls` no longer lists it"
        );
        tokio::time::sleep(START_POLL).await;
    }
    Ok(())
}

/// The demo `name` and the command after `--` select: a command names an
/// ad hoc demo, otherwise `name` or the only configured demo is the one.
fn choose(
    configured: &BTreeMap<String, Demo>,
    name: Option<String>,
    command: Vec<OsString>,
) -> Result<Chosen> {
    if !command.is_empty() {
        let name = name.unwrap_or_else(|| DEFAULT_NAME.to_owned());
        crate::demos::validate_name(&name)?;
        let demo = configured.get(&name).cloned().unwrap_or_else(|| Demo {
            cmd: Vec::new(),
            preset: None,
            open: None,
            scheme: None,
            ports: None,
        });
        return Ok(Chosen {
            name,
            demo,
            command,
        });
    }
    let names = || configured.keys().cloned().collect::<Vec<_>>().join(", ");
    let (name, demo) = match name {
        Some(name) => {
            let demo = configured.get(&name).with_context(|| {
                if configured.is_empty() {
                    format!("no demo named {name} is configured; pass its command after --")
                } else {
                    format!(
                        "no demo named {name} is configured; configured demos: {}",
                        names()
                    )
                }
            })?;
            (name, demo.clone())
        }
        None => {
            let mut demos = configured.iter();
            match (demos.next(), demos.next()) {
                (Some((name, demo)), None) => (name.clone(), demo.clone()),
                (None, _) => bail!("no demo is configured; pass a command after --"),
                (Some(_), Some(_)) => bail!("name the demo to start: {}", names()),
            }
        }
    };
    let command = demo.cmd.iter().map(OsString::from).collect();
    Ok(Chosen {
        name,
        demo,
        command,
    })
}

impl Chosen {
    fn with(mut self, overrides: Overrides) -> Self {
        if overrides.preset.is_some() {
            self.demo.preset = overrides.preset;
        }
        if overrides.scheme.is_some() {
            self.demo.scheme = overrides.scheme;
        }
        if let Some(open) = overrides.open {
            self.demo.open = Some(vec!["sh".into(), "-c".into(), open]);
        }
        if !overrides.ports.is_empty() {
            self.demo.ports = Some(overrides.ports);
        }
        self
    }
}

/// Start the demo's worker in a terminal host and return what it records.
async fn place(
    ctx: &Context,
    settings: &crate::config::Effective,
    workspace: &Workspace,
    chosen: &Chosen,
    provided: presets::Provided,
) -> Result<Launch> {
    // Places can repeat, such as a background demo's log, so the start finds
    // its own record by this launch's ID.
    let id = uuid::Uuid::new_v4().to_string();
    let launch = |place: &Origin| Launch {
        id: id.clone(),
        name: chosen.name.clone(),
        preset: chosen.demo.preset,
        open: chosen.demo.open.clone(),
        scheme: chosen.demo.scheme,
        environment: provided.environment.clone(),
        origin: place.clone(),
    };
    let worker = |place: &Origin| {
        let launch = serde_json::to_string(&launch(place))?;
        internal_command(
            &ctx.paths,
            false,
            Worker::Demo {
                workspace: &workspace.id,
                launch: &launch,
                command: &chosen.command,
            },
        )
    };
    let place = terminal::start_demo(ctx, settings, workspace, &chosen.name, &id, &worker).await?;
    Ok(launch(&place))
}

/// Wait until the worker has recorded `launch`, which shows its command runs.
async fn await_record(ctx: &Context, workspace: &Workspace, launch: &Launch) -> Result<Running> {
    let deadline = tokio::time::Instant::now() + START_LIMIT;
    loop {
        let progress = demos::progress(
            &ctx.paths,
            SpaceSelector::id(&workspace.id),
            launch.id.clone(),
        )
        .await?;
        match progress {
            Progress::Running { demo } => return Ok(*demo),
            Progress::Ended => return Err(not_started(launch).await),
            Progress::Starting if tokio::time::Instant::now() >= deadline => {
                return Err(not_started(launch).await);
            }
            Progress::Starting => tokio::time::sleep(START_POLL).await,
        }
    }
}

/// Why the demo did not start, from the end of what its place shows.
async fn not_started(launch: &Launch) -> anyhow::Error {
    let screen = terminal::demo_screen(launch).await.unwrap_or_default();
    anyhow::anyhow!(
        "demo {} did not start{}",
        launch.name,
        last_lines(&screen, 20)
            .map(|lines| format!(":\n{lines}"))
            .unwrap_or_default()
    )
}

#[cfg(test)]
mod tests;
