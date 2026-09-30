//! Manual recovery uses saved agent identity and today's restore configuration.
use crate::{
    cli::{
        client,
        context::Context,
        ui::{self, Fallback},
    },
    execution::recovery::{Record, Recovery},
    paths::Paths,
};
use anyhow::{Context as _, Result, ensure};
use std::path::PathBuf;

pub async fn run(
    ctx: &Context,
    selector: Option<String>,
    execution: Option<String>,
    discard: bool,
) -> Result<i32> {
    ensure!(
        !crate::env::is_scoped(),
        "resume agents outside scoped executions"
    );
    let id = ui::select_workspace(ctx, selector, Fallback::CurrentDirectory).await?;
    let inspection = client::inspect(&ctx.paths, id).await?;
    let records = records(&ctx.paths, &inspection.workspace.id, execution.as_deref())?;
    let selected = match records.as_slice() {
        [] => anyhow::bail!("no agent recovery record for this workspace"),
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
    let selected_id = selected
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".recovery.json"))
        .context("invalid recovery filename")?;
    ensure!(
        !inspection
            .executions
            .iter()
            .any(|execution| execution.id == selected_id),
        "selected execution is active or unknown; stop or reconcile it first"
    );
    let _claim = claim(&selected)?;
    if discard {
        std::fs::remove_file(&selected)?;
        ctx.show(&selected_id, |id| {
            println!("Discarded recovery record for {id}")
        })?;
        return Ok(0);
    }
    let recorded: Record =
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
    crate::execution::run_recovery(
        &ctx.paths,
        inspection.workspace.id,
        recovery.command,
        recovery.agent,
        selected,
    )
    .await
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

fn claim(path: &std::path::Path) -> Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?;
    fs2::FileExt::try_lock_exclusive(&file)
        .context("this recovery record is already being resumed")?;
    ensure!(path.try_exists()?, "recovery record was already consumed");
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recovery_record_cannot_be_claimed_twice() {
        let root = tempfile::tempdir().unwrap();
        let record = root.path().join("recovery.json");
        std::fs::write(&record, "{}").unwrap();
        let first = claim(&record).unwrap();
        assert!(claim(&record).is_err());
        std::fs::remove_file(&record).unwrap();
        drop(first);
        assert!(claim(&record).is_err());
    }
}
