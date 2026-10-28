//! `shoal demo`: commands kept running in a workspace for the user to try.
//! An agent starts one, the user opens it, and both can read its screen.
use std::{collections::BTreeMap, ffi::OsString, time::Duration};

use anyhow::{Context as _, Result, bail, ensure};
use serde::Serialize;
use serde_json::json;

use crate::{
    cli::{
        args::sessions::DemoCommand,
        client,
        context::Context,
        internal::{Worker, internal_command},
        output::{Palette, Style},
        terminal,
        ui::{self, Fallback},
    },
    demos::{Demo, Launch, Preset, Progress, Running},
    model::Workspace,
    protocol::{ConfigTarget, DemoMethod, NotificationMethod, WorkspaceMethod},
    sessions::Origin,
};

mod presets;

/// The name an ad hoc demo takes when the agent gives none.
const DEFAULT_NAME: &str = "demo";
/// How often a start checks that a replaced demo ended and its worker
/// recorded the new one.
const START_POLL: Duration = Duration::from_millis(100);
/// A hang guard for a demo that never ends or a worker that never records it.
const START_LIMIT: Duration = Duration::from_secs(120);

pub(super) async fn run(
    ctx: &Context,
    command: Option<DemoCommand>,
    name: Option<String>,
    workspace: Option<String>,
) -> Result<i32> {
    // Without its token, the daemon would answer a workspace process as the user.
    ensure!(
        crate::env::is_scoped() || !crate::env::inherits_scope(&ctx.paths.state),
        "a workspace process that cleared its scope token cannot use shoal demo"
    );
    // Before a picker opens or anything is stopped or replaced.
    if let Some(name) = requested_name(command.as_ref(), name.as_deref()) {
        crate::demos::validate_name(name)?;
    }
    match command {
        None => open(ctx, name, workspace).await,
        Some(DemoCommand::Start {
            name,
            preset,
            scheme,
            open,
            ports,
            queue,
            workspace,
            command,
        }) => {
            let overrides = Overrides {
                preset,
                scheme,
                open,
                ports,
            };
            start(ctx, workspace, name, overrides, queue, command).await
        }
        Some(DemoCommand::Stop { name, workspace }) => stop(ctx, workspace, name).await,
        Some(DemoCommand::Ls { workspace }) => list(ctx, workspace).await,
        Some(DemoCommand::Screen {
            name,
            lines,
            workspace,
        }) => screen(ctx, workspace, &name, lines).await,
        Some(DemoCommand::Send {
            name,
            keys,
            workspace,
        }) => send(ctx, workspace, &name, &keys).await,
    }
}

/// The demo name the command line gives, if it gives one.
fn requested_name<'a>(command: Option<&'a DemoCommand>, name: Option<&'a str>) -> Option<&'a str> {
    match command {
        None => name,
        Some(DemoCommand::Start { name, .. } | DemoCommand::Stop { name, .. }) => name.as_deref(),
        Some(DemoCommand::Screen { name, .. } | DemoCommand::Send { name, .. }) => Some(name),
        Some(DemoCommand::Ls { .. }) => None,
    }
}

/// What `shoal demo start` replaces in the configured demo.
struct Overrides {
    preset: Option<Preset>,
    scheme: Option<crate::demos::Scheme>,
    open: Option<String>,
    ports: Vec<String>,
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
struct Chosen {
    name: String,
    demo: Demo,
    command: Vec<OsString>,
}

async fn start(
    ctx: &Context,
    workspace: Option<String>,
    name: Option<String>,
    overrides: Overrides,
    queue: bool,
    command: Vec<OsString>,
) -> Result<i32> {
    // Before a picker opens or a running demo is replaced.
    overrides.validate()?;
    let id = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let workspace = client::inspect(&ctx.paths, id).await?.workspace;
    let settings =
        client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.id.clone())).await?;
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
        client::call(
            &ctx.paths,
            NotificationMethod::SendMessage {
                workspace: workspace.id.clone(),
                message,
            },
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
        let progress = client::request::<Progress>(
            &ctx.paths,
            DemoMethod::DemoProgress {
                workspace: workspace.id.clone(),
                launch: launch.id.clone(),
            },
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

async fn stop(ctx: &Context, workspace: Option<String>, name: Option<String>) -> Result<i32> {
    let id = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let workspace = client::inspect(&ctx.paths, id).await?.workspace;
    stop_named(ctx, &workspace, name.clone()).await?;
    let message = match &name {
        Some(name) => format!("Stopping demo {name}"),
        None => format!("Stopping the demos of {}", workspace.name),
    };
    ctx.emit_styled(
        Style::Success,
        &message,
        json!({"stopping": true, "name": name}),
    )?;
    Ok(0)
}

/// Stop a running demo `name` and wait for it to end, so the new one neither
/// shares its place nor is mistaken for it.
async fn replace(ctx: &Context, workspace: &Workspace, name: &str) -> Result<()> {
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

async fn stop_named(ctx: &Context, workspace: &Workspace, name: Option<String>) -> Result<()> {
    client::request::<()>(
        &ctx.paths,
        DemoMethod::StopDemos {
            workspace: workspace.id.clone(),
            name,
        },
    )
    .await
}

/// Configured and running demos, as `shoal demo ls --json` prints them.
#[derive(Serialize)]
struct Listing {
    workspace: Option<String>,
    configured: BTreeMap<String, Demo>,
    running: Vec<Running>,
}

async fn list(ctx: &Context, workspace: Option<String>) -> Result<i32> {
    let workspace = scope(ctx, workspace).await?;
    let configured = match &workspace {
        Some(workspace) => {
            client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.id.clone()))
                .await?
                .demos
        }
        None => BTreeMap::new(),
    };
    let listing = Listing {
        workspace: workspace.as_ref().map(|workspace| workspace.name.clone()),
        running: running(ctx, workspace.as_ref()).await?,
        configured,
    };
    let rows = listing_rows(&listing, Palette::stdout(ctx.json));
    ctx.show(&listing, |_| {
        for row in &rows {
            outln!("{row}");
        }
        if rows.is_empty() {
            outln!("No demos");
        }
    })?;
    Ok(0)
}

/// One row per demo: its name, workspace, state and preset; configured demos
/// that are not running follow the running ones.
fn listing_rows(listing: &Listing, palette: Palette) -> Vec<String> {
    let preset = |preset: Option<Preset>| preset.map(Preset::as_str).unwrap_or_default().to_owned();
    let mut rows: Vec<[ui::Cell; 4]> = listing
        .running
        .iter()
        .map(|demo| {
            [
                (demo.launch.name.clone(), Some(Style::Heading)),
                (demo.workspace.clone(), None),
                ("Running".into(), Some(Style::Success)),
                (preset(demo.launch.preset), Some(Style::Muted)),
            ]
        })
        .collect();
    for (name, demo) in &listing.configured {
        if listing
            .running
            .iter()
            .any(|running| running.launch.name == *name)
        {
            continue;
        }
        rows.push([
            (name.clone(), Some(Style::Heading)),
            (listing.workspace.clone().unwrap_or_default(), None),
            ("Not running".into(), Some(Style::Muted)),
            (preset(demo.preset), Some(Style::Muted)),
        ]);
    }
    ui::aligned(rows, palette)
}

async fn open(ctx: &Context, name: Option<String>, workspace: Option<String>) -> Result<i32> {
    ensure!(ctx.interactive(), "opening a demo requires a terminal");
    let workspace = scope(ctx, workspace).await?;
    let demos = running(ctx, workspace.as_ref()).await?;
    let demo = match name {
        Some(name) => find(demos, &name, workspace.as_ref())?,
        None => pick(ctx, demos)?,
    };
    open_running(ctx, &demo).await?;
    Ok(0)
}

fn pick(ctx: &Context, demos: Vec<Running>) -> Result<Running> {
    ensure!(!demos.is_empty(), "no demos are running");
    let rows = ui::aligned(
        demos
            .iter()
            .map(|demo| {
                [
                    (demo.launch.name.clone(), None),
                    (demo.workspace.clone(), None),
                ]
            })
            .collect(),
        Palette::picker(),
    );
    let choices: Vec<_> = demos
        .iter()
        .zip(&rows)
        .map(|(demo, row)| (demo, row.as_str()))
        .collect();
    ui::pick_choice(ctx, "Open demo> ", &choices).cloned()
}

/// Open a running demo in this terminal: its `open` command, its preset's
/// way, or else the host's view of it.
pub(in crate::cli) async fn open_running(ctx: &Context, demo: &Running) -> Result<()> {
    match (&demo.launch.open, demo.launch.preset) {
        (Some(open), _) => run_opener(ctx, demo, open).await,
        (None, Some(preset)) => presets::open(preset, &demo.launch),
        (None, None) => terminal::open_demo(&demo.launch).await,
    }
}

/// Run the demo's `open` command in its workspace with the demo's variables,
/// once the worktree is verified to be the recorded one.
async fn run_opener(ctx: &Context, demo: &Running, open: &[String]) -> Result<()> {
    let workspace: Workspace = client::request(
        &ctx.paths,
        WorkspaceMethod::VerifyWorkspace {
            workspace: demo.workspace_id.clone(),
            on_branch: false,
        },
    )
    .await?;
    let mut command = tokio::process::Command::new(&open[0]);
    command
        .args(&open[1..])
        .current_dir(&workspace.path)
        .envs(&demo.launch.environment);
    let status = crate::execution::run_in_terminal(command)
        .await
        .with_context(|| format!("run the open command of demo {}", demo.launch.name))?;
    ensure!(
        status.success(),
        "the open command of demo {} exited with {status}",
        demo.launch.name
    );
    Ok(())
}

async fn screen(ctx: &Context, workspace: Option<String>, name: &str, lines: usize) -> Result<i32> {
    let demo = named(ctx, workspace, name).await?;
    let screen = terminal::demo_screen(&demo.launch).await?;
    let text = last_lines(&screen, lines).unwrap_or_default();
    ctx.show(&json!({"name": name, "screen": text}), |_| outln!("{text}"))?;
    Ok(0)
}

async fn send(ctx: &Context, workspace: Option<String>, name: &str, keys: &str) -> Result<i32> {
    let demo = named(ctx, workspace, name).await?;
    terminal::send_demo_keys(&demo.launch, keys).await?;
    ctx.emit(&format!("Sent to demo {name}"), json!({"sent": true}))?;
    Ok(0)
}

/// The running demo `name` of the workspace given or chosen.
async fn named(ctx: &Context, workspace: Option<String>, name: &str) -> Result<Running> {
    let id = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let workspace = client::inspect(&ctx.paths, id).await?.workspace;
    find(
        running(ctx, Some(&workspace)).await?,
        name,
        Some(&workspace),
    )
}

fn find(demos: Vec<Running>, name: &str, workspace: Option<&Workspace>) -> Result<Running> {
    let mut matching: Vec<_> = demos
        .into_iter()
        .filter(|demo| demo.launch.name == name)
        .collect();
    match (matching.len(), workspace) {
        (0, Some(workspace)) => bail!("{} has no running demo {name}", workspace.name),
        (0, None) => bail!("no demo {name} is running"),
        (1, _) => Ok(matching.remove(0)),
        _ => bail!("several workspaces run a demo {name}; pass --workspace"),
    }
}

/// The workspace whose demos to show: the one named, the current one, a
/// workspace process's own, or `None` for every workspace.
async fn scope(ctx: &Context, workspace: Option<String>) -> Result<Option<Workspace>> {
    let id = match workspace {
        Some(selector) => Some(ui::explicit_workspace(ctx, selector).await?),
        // A workspace process selects as start and stop do: the directory's
        // workspace, which may be a child of its own, otherwise its own.
        None if crate::env::is_scoped() => {
            Some(ui::select_workspace(ctx, None, Fallback::CurrentDirectoryOnly).await?)
        }
        None => {
            let workspaces = client::workspaces(&ctx.paths).await?;
            ui::current_workspace(&workspaces)?.map(|workspace| workspace.id.clone())
        }
    };
    match id {
        Some(id) => Ok(Some(client::inspect(&ctx.paths, id).await?.workspace)),
        None => Ok(None),
    }
}

async fn running(ctx: &Context, workspace: Option<&Workspace>) -> Result<Vec<Running>> {
    client::request(
        &ctx.paths,
        DemoMethod::ListDemos {
            workspace: workspace.map(|workspace| workspace.id.clone()),
        },
    )
    .await
}

/// The last `count` lines of `screen` with trailing blank lines dropped.
fn last_lines(screen: &str, count: usize) -> Option<String> {
    let lines: Vec<&str> = screen.trim_end().lines().collect();
    let start = lines.len().saturating_sub(count);
    let text = lines[start..].join("\n");
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests;
