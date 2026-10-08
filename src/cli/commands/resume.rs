//! Manual recovery uses saved agent identity and today's restore configuration.
use crate::{
    cli::{
        client,
        context::Context,
        ui::{self, Fallback},
    },
    execution::recovery::{Record, Recovery, Saved, SavedCommand, consume},
    model::Workspace,
};
use anyhow::{Context as _, Result, ensure};
use serde_json::json;

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
    let workspace = &inspection.workspace;
    let mut saved = Saved::load(&ctx.paths, &workspace.id)?;
    if let Some(execution) = &execution {
        saved.retain_execution(execution, !discard);
    }
    ensure!(
        !saved.is_empty(),
        "no stopped agents or commands for this workspace"
    );
    let active = |id: &str| inspection.executions.iter().any(|e| e.id == id);
    if discard {
        return discard_saved(ctx, workspace, &saved, active);
    }
    let selected = match saved.agents.as_slice() {
        [] => return report_commands(ctx, workspace, &saved.commands),
        [(_, path)] => path.clone(),
        agents if ctx.interactive() => {
            let choices = agents
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
        !active(selected_id),
        "selected execution is active or unknown; stop or reconcile it first"
    );
    let _claim = claim(&selected)?;
    let recorded: Record =
        serde_json::from_slice(&std::fs::read(&selected)?).context("read recovery record")?;
    if let Some(reason) = &recorded.stop_reason {
        eprintln!("shoal: restoring {} stopped after {reason}", recorded.agent);
    }
    let handoff = handoff(&saved.commands);
    let recovery =
        Recovery::resolve(&ctx.paths, workspace, &recorded.agent, handoff.as_deref()).await?;
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
    if !saved.commands.is_empty() && !recovery.handoff_delivered {
        eprintln!("{}", command_list(workspace, &saved.commands));
    }
    let mut records = vec![selected];
    records.extend(saved.commands.into_iter().map(|command| command.path));
    crate::execution::run_recovery(
        &ctx.paths,
        workspace.id.clone(),
        recovery.command,
        recovery.agent,
        records,
    )
    .await
}

fn discard_saved(
    ctx: &Context,
    workspace: &Workspace,
    saved: &Saved,
    active: impl Fn(&str) -> bool,
) -> Result<i32> {
    let mut claims = Vec::new();
    for (id, path) in &saved.agents {
        ensure!(
            !active(id),
            "stopped agent {id} is active or unknown; stop or reconcile it first"
        );
        claims.push(claim(path)?);
    }
    for (_, path) in &saved.agents {
        std::fs::remove_file(path)?;
    }
    for command in &saved.commands {
        consume(&command.path)?;
    }
    let ids = saved
        .agents
        .iter()
        .map(|(id, _)| id.as_str())
        .chain(saved.commands.iter().map(|command| command.id.as_str()))
        .collect::<Vec<_>>();
    ctx.show(&json!({"discarded": ids}), |_| {
        println!(
            "Discarded stopped agents and commands for {}",
            workspace.name
        )
    })?;
    Ok(0)
}

/// Without an agent to receive them, interrupted commands go to the user once.
fn report_commands(ctx: &Context, workspace: &Workspace, commands: &[SavedCommand]) -> Result<i32> {
    let argv = commands
        .iter()
        .map(|command| &command.argv)
        .collect::<Vec<_>>();
    ctx.show(
        &json!({"workspace": workspace.name, "commands": argv}),
        |_| println!("{}", command_list(workspace, commands)),
    )?;
    for command in commands {
        consume(&command.path)?;
    }
    Ok(0)
}

fn command_list(workspace: &Workspace, commands: &[SavedCommand]) -> String {
    let mut text = format!("Stopped commands in {}, not restarted:", workspace.name);
    for command in commands {
        let mut words = vec!["shoal", "exec", &workspace.name, "--"];
        words.extend(command.argv.iter().map(String::as_str));
        text.push_str(&format!("\n  {}", crate::shell::quote(&words)));
    }
    text
}

/// The first prompt of a restored session, naming the commands stopped with it.
fn handoff(commands: &[SavedCommand]) -> Option<String> {
    if commands.is_empty() {
        return None;
    }
    let mut text = String::from(
        "Shoal stopped this session. These workspace commands were stopped too and were not restarted:",
    );
    for command in commands {
        text.push_str(&format!("\n- {}", crate::shell::quote(&command.argv)));
    }
    text.push_str("\nRerun the ones that are still needed.");
    Some(text)
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
