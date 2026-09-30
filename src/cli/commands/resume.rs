//! Manual recovery uses saved agent identity and today's restore configuration.
use crate::{
    cli::{
        client,
        context::Context,
        ui::{self, Fallback},
    },
    execution::recovery::Recovery,
    paths::Paths,
};
use anyhow::{Context as _, Result, ensure};
use std::path::PathBuf;

pub async fn run(
    ctx: &Context,
    selector: Option<String>,
    execution: Option<String>,
) -> Result<i32> {
    ensure!(
        !crate::env::is_scoped(),
        "resume agents outside scoped executions"
    );
    let id = ui::select_workspace(ctx, selector, Fallback::CurrentDirectory).await?;
    let inspection = client::inspect(&ctx.paths, id).await?;
    ensure!(
        inspection.executions.is_empty(),
        "workspace has active or unknown executions; stop or reconcile them before resuming"
    );
    let records = records(&ctx.paths, &inspection.workspace.id, execution.as_deref())?;
    let selected = match records.as_slice() {
        [] => anyhow::bail!("no overload recovery record for this workspace"),
        [(_, path)] => path.clone(),
        _ if ctx.interactive() => {
            let choices = records
                .iter()
                .map(|(id, path)| (path.clone(), id.as_str()))
                .collect::<Vec<_>>();
            ui::pick_choice(ctx, "Resume execution> ", &choices)?
        }
        _ => anyhow::bail!("multiple stopped agents; select one with --execution <id>"),
    };
    let recorded: Recovery =
        serde_json::from_slice(&std::fs::read(&selected)?).context("read recovery record")?;
    let recovery = Recovery::resolve(&ctx.paths, &inspection.workspace, &recorded.agent).await?;
    ensure!(
        !recovery.command.is_empty(),
        "configure [agent_resume].{} with a session restore command",
        recovery.agent
    );
    if !recovery.automatic {
        ensure!(
            ctx.interactive(),
            "session picker requires a terminal; configure [agent_resume].{} for noninteractive recovery",
            recovery.agent
        );
    }
    let code = crate::execution::run(
        &ctx.paths,
        inspection.workspace.id,
        recovery.command,
        Some(recovery.agent),
    )
    .await?;
    if code == 0 {
        std::fs::remove_file(selected)?;
    }
    Ok(code)
}

fn records(
    paths: &Paths,
    workspace_id: &str,
    execution: Option<&str>,
) -> Result<Vec<(String, PathBuf)>> {
    let directory = paths.workspace_state(workspace_id);
    let mut records = Vec::new();
    if directory.is_dir() {
        for entry in std::fs::read_dir(directory)? {
            let path = entry?.path();
            let Some(id) = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_suffix(".recovery.json"))
            else {
                continue;
            };
            if execution.is_none_or(|requested| requested == id) {
                records.push((id.to_owned(), path));
            }
        }
    }
    records.sort();
    Ok(records)
}
