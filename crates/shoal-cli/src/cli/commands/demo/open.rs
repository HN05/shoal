//! `shoal demo [name]`: opening a running demo, named or picked, in this
//! terminal.
use anyhow::{Context as _, Result, ensure};

use super::{
    lookup::{find, running, scope},
    presets,
};
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{context::Context, output::Palette, terminal, ui},
    demos::Running,
};

pub(super) async fn open(
    ctx: &Context,
    name: Option<String>,
    workspace: Option<SpaceSelector>,
) -> Result<i32> {
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
                    (demo.workspace.to_string(), None),
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
async fn open_running(ctx: &Context, demo: &Running) -> Result<()> {
    match (&demo.launch.open, demo.launch.preset) {
        (Some(open), _) => run_opener(ctx, demo, open).await,
        (None, Some(preset)) => presets::open(preset, &demo.launch),
        (None, None) => terminal::open_demo(&demo.launch).await,
    }
}

/// Run the demo's `open` command in its workspace with the demo's variables,
/// once the worktree is verified to be the recorded one.
async fn run_opener(ctx: &Context, demo: &Running, open: &[String]) -> Result<()> {
    let workspace =
        crate::client::workspaces::verify(&ctx.paths, SpaceSelector::id(&demo.workspace_id), false)
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
