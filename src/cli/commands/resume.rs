//! Manual recovery uses saved agent identity and today's restore configuration.
use crate::{
    cli::{
        client,
        context::Context,
        herdr,
        ui::{self, Fallback},
    },
    config::Herdr,
    execution::recovery::{Record, Recovery, Saved, SavedCommand, consume},
    model::Workspace,
    protocol::ConfigTarget,
};
use anyhow::{Context as _, Result, ensure};
use serde_json::json;
use std::ffi::OsString;

pub async fn run(
    ctx: &Context,
    selector: Option<String>,
    execution: Option<String>,
    discard: bool,
) -> Result<i32> {
    ensure!(
        !crate::env::inherits_scope(&ctx.paths.state),
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
        let ids = discard_saved(&saved, active)?;
        ctx.show(&json!({"discarded": ids}), |_| {
            println!(
                "Discarded stopped agents and commands for {}",
                workspace.name
            )
        })?;
        return Ok(0);
    }
    let _commands = claim_commands(&mut saved.commands);
    let selected = match saved.agents.as_slice() {
        [] => {
            let report = report_commands(ctx, workspace, &saved.commands)?;
            ctx.show(&report, |_| {})?;
            return Ok(0);
        }
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

/// Forget saved records, returning their execution IDs.
fn discard_saved(saved: &Saved, active: impl Fn(&str) -> bool) -> Result<Vec<String>> {
    let mut claims = Vec::new();
    for (id, path) in &saved.agents {
        ensure!(
            !active(id),
            "stopped agent {id} is active or unknown; stop or reconcile it first"
        );
        claims.push(claim(path)?);
    }
    for command in &saved.commands {
        claims.push(claim(&command.path)?);
    }
    for (_, path) in &saved.agents {
        std::fs::remove_file(path)?;
    }
    for command in &saved.commands {
        consume(&command.path)?;
    }
    Ok(saved
        .agents
        .iter()
        .map(|(id, _)| id.clone())
        .chain(saved.commands.iter().map(|command| command.id.clone()))
        .collect())
}

/// Resume each workspace's stopped work. Commands without an agent are
/// reported here. Inside Herdr an agent resumes in the pane it stopped in
/// when that pane is back at its prompt; otherwise one agent resumes in this
/// terminal, several in new Herdr tabs, and without Herdr each is listed for
/// its own terminal.
pub async fn run_all(ctx: &Context, discard: bool) -> Result<i32> {
    ensure!(
        !crate::env::inherits_scope(&ctx.paths.state),
        "resume agents outside scoped executions"
    );
    let mut stopped = Vec::new();
    for workspace in client::workspaces(&ctx.paths).await? {
        let saved = Saved::load(&ctx.paths, &workspace.id)?;
        if !saved.is_empty() {
            stopped.push((workspace, saved));
        }
    }
    if stopped.is_empty() {
        ctx.emit(
            "No stopped agents or commands",
            json!({"commands": [], "agents": []}),
        )?;
        return Ok(0);
    }
    if discard {
        let mut ids = Vec::new();
        for (workspace, saved) in &stopped {
            let inspection = client::inspect(&ctx.paths, workspace.id.clone()).await?;
            let active = |id: &str| inspection.executions.iter().any(|e| e.id == id);
            ids.extend(discard_saved(saved, active)?);
        }
        let names = stopped.iter().map(|(workspace, _)| workspace.name.as_str());
        let names = names.collect::<Vec<_>>().join(", ");
        ctx.show(&json!({"discarded": ids}), |_| {
            println!("Discarded stopped agents and commands for {names}")
        })?;
        return Ok(0);
    }
    let mut commands = Vec::new();
    let mut sessions = Vec::new();
    for (workspace, mut saved) in stopped {
        if saved.agents.is_empty() {
            let _claims = claim_commands(&mut saved.commands);
            commands.push(report_commands(ctx, &workspace, &saved.commands)?);
        } else {
            sessions.extend(
                saved
                    .agents
                    .into_iter()
                    .map(|(id, path)| (workspace.clone(), id, path)),
            );
        }
    }
    let single = sessions.len() == 1 && ctx.interactive();
    let mut agents = Vec::new();
    for (workspace, id, record) in &sessions {
        let integration = herdr_settings(ctx, workspace).await?;
        let args = ["resume", &workspace.id, "--execution", id].map(OsString::from);
        let reused = integration.is_some() && resume_in_recorded_pane(ctx, record, &args).await?;
        if single && !reused {
            return run(ctx, Some(workspace.id.clone()), Some(id.clone()), false).await;
        }
        let tabs = reused || integration.is_some_and(|settings| settings.new_tab);
        if tabs && !reused {
            let label = launch_label(ctx, workspace).await;
            herdr::run_in_tab(ctx, &workspace.path, &label, &args).await?;
        }
        agents.push(json!({
            "workspace": workspace.name,
            "execution": id,
            "tab": tabs,
            "reused": reused,
        }));
    }
    ctx.show(&json!({"commands": commands, "agents": agents}), |_| {
        let listed = agents.iter().filter(|agent| agent["tab"] == false).count();
        if listed < sessions.len() {
            println!("Resuming {} agents in Herdr tabs", sessions.len() - listed);
        }
        if listed > 0 {
            println!("Resume each agent in its own terminal:");
        }
        for ((workspace, id, _), agent) in sessions.iter().zip(&agents) {
            if agent["tab"] == false {
                let words = ["shoal", "resume", &workspace.name, "--execution", id];
                println!("  {}", crate::shell::quote(&words));
            }
        }
    })?;
    Ok(0)
}

/// The workspace's Herdr settings when this terminal is a Herdr pane and the
/// integration is enabled.
async fn herdr_settings(ctx: &Context, workspace: &Workspace) -> Result<Option<Herdr>> {
    if !herdr::available(ctx) {
        return Ok(None);
    }
    let settings =
        client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.id.clone())).await?;
    Ok(Some(settings.herdr).filter(|herdr| herdr.enabled))
}

/// Restore the agent in the Herdr pane it stopped in, unless that pane is
/// this terminal, busy or gone.
async fn resume_in_recorded_pane(
    ctx: &Context,
    record: &std::path::Path,
    args: &[OsString],
) -> Result<bool> {
    let pane = std::fs::read(record)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Record>(&bytes).ok())
        .and_then(|record| record.herdr_pane)
        .filter(|pane| herdr::current_pane().as_ref() != Some(pane));
    match pane {
        Some(pane) => herdr::run_at_prompt(ctx, &pane, args).await,
        None => Ok(false),
    }
}

/// The tab label `shoal add` would give the workspace. A failed issue lookup
/// falls back to the branch.
async fn launch_label(ctx: &Context, workspace: &Workspace) -> String {
    let labelled = async {
        let settings =
            client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.id.clone())).await?;
        let repositories = client::repositories(&ctx.paths).await?;
        let repo = repositories
            .iter()
            .find(|repo| repo.id == workspace.repository_id)
            .context("repository is no longer registered")?;
        let issue = match &workspace.links.issue {
            Some(url) => Some(super::issues::load(repo, url).await?),
            None => None,
        };
        anyhow::Ok(herdr::tab_label(
            settings.herdr.tab_name,
            repo,
            issue.as_ref(),
            &workspace.branch,
        ))
    };
    labelled.await.unwrap_or_else(|error| {
        eprintln!("warning: cannot label the Herdr tab: {error:#}");
        workspace.branch.clone()
    })
}

/// Only one resume reports a command; another holding its lock skips it.
fn claim_commands(commands: &mut Vec<SavedCommand>) -> Vec<std::fs::File> {
    let mut claims = Vec::new();
    commands.retain(|command| match claim(&command.path) {
        Ok(file) => {
            claims.push(file);
            true
        }
        Err(_) => false,
    });
    claims
}

/// Without an agent to receive them, interrupted commands go to the user once.
fn report_commands(
    ctx: &Context,
    workspace: &Workspace,
    commands: &[SavedCommand],
) -> Result<serde_json::Value> {
    let argv = commands
        .iter()
        .map(|command| &command.argv)
        .collect::<Vec<_>>();
    let report = json!({"workspace": workspace.name, "commands": argv});
    if !commands.is_empty() && !ctx.json {
        println!("{}", command_list(workspace, commands));
    }
    for command in commands {
        consume(&command.path)?;
    }
    Ok(report)
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

    #[test]
    fn discard_leaves_command_records_another_resume_is_reporting() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("execution.command.json");
        std::fs::write(&path, r#"{"argv":["true"]}"#).unwrap();
        let saved = Saved {
            agents: vec![],
            commands: vec![SavedCommand {
                id: "execution".into(),
                path: path.clone(),
                argv: vec!["true".into()],
            }],
        };
        let reporting = claim(&path).unwrap();
        assert!(discard_saved(&saved, |_| false).is_err());
        assert!(path.exists());
        drop(reporting);
        assert_eq!(discard_saved(&saved, |_| false).unwrap(), ["execution"]);
        assert!(!path.exists());
    }
}
