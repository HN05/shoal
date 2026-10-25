//! Manual recovery uses saved agent identity and today's restore configuration.
use crate::{
    cli::{WorkspaceScope, client, context::Context, terminal, ui},
    daemon::recovery::{ReconcileOptions, Report},
    execution::recovery::{
        LastSession, Record, Recovery, RecoveryClaim, Saved, SavedCommand, claim, consume,
        discard_saved,
    },
    model::{Inspection, Workspace},
    protocol::{ConfigTarget, Method},
    state::ExecutionState,
};
use anyhow::{Context as _, Result, ensure};
use serde_json::json;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

/// `shoal resume`: the selected workspace, or every one for `--all`, the
/// picker's entry for all, or when no workspace has stopped work.
pub async fn select(
    ctx: &Context,
    scope: WorkspaceScope,
    execution: Option<String>,
    discard: bool,
) -> Result<i32> {
    ensure!(
        !crate::env::inherits_scope(&ctx.paths.state),
        "resume agents outside scoped executions"
    );
    match ui::select_stopped_workspace(ctx, scope).await? {
        None => run_all(ctx, discard).await,
        Some(workspace) => run(ctx, workspace, execution, discard).await,
    }
}

pub async fn run(
    ctx: &Context,
    workspace: String,
    execution: Option<String>,
    discard: bool,
) -> Result<i32> {
    let mut inspection = client::inspect(&ctx.paths, workspace).await?;
    let workspace_id = inspection.workspace.id.clone();
    let mut saved = Saved::load(&ctx.paths, &workspace_id)?;
    if let Some(execution) = &execution {
        saved.retain_execution(execution, !discard);
    }
    if saved.is_empty()
        && execution.is_none()
        && let Some(last) = LastSession::load(&ctx.paths, &workspace_id)?
    {
        return continue_last_session(ctx, &inspection.workspace, last, discard).await;
    }
    ensure!(
        !saved.is_empty(),
        "no stopped agents or commands for this workspace"
    );
    let recovery_id = execution
        .as_deref()
        .or_else(|| (saved.agents.len() == 1).then(|| saved.agents[0].0.as_str()));
    if !discard {
        reconcile_stale_execution(ctx, &inspection, recovery_id).await?;
    }
    inspection = client::inspect(&ctx.paths, workspace_id).await?;
    let workspace = &inspection.workspace;
    let active = |id: &str| inspection.executions.iter().any(|e| e.id == id);
    if discard {
        let ids = discard_saved(&saved, active)?;
        ctx.show(&json!({"discarded": ids}), |_| {
            outln!(
                "Discarded stopped agents and commands for {}",
                workspace.name
            )
        })?;
        return Ok(0);
    }
    let _commands = claim_commands(&mut saved.commands);
    let (selected_id, selected) = match saved.agents.as_slice() {
        [] => {
            let report = report_commands(ctx, workspace, &saved.commands)?;
            ctx.show(&report, |_| {})?;
            return Ok(0);
        }
        agents => select_agent(ctx, agents)?,
    };
    ensure!(
        !active(&selected_id),
        "selected execution is active or unknown; stop or reconcile it first"
    );
    let _claim = claim(&selected)?;
    let recorded = Record::load(&selected)?.context("recovery record was already consumed")?;
    if let Some(reason) = &recorded.stop_reason {
        eprintln!("shoal: restoring {} stopped after {reason}", recorded.agent);
    }
    let handoff = handoff(&saved.commands);
    if recorded.moved {
        let records = std::iter::once(selected)
            .chain(saved.commands.into_iter().map(|command| command.path))
            .collect();
        return start_moved(ctx, workspace, &recorded.agent, handoff.as_deref(), records).await;
    }
    let recovery = Recovery::resolve(
        &ctx.paths,
        workspace,
        &recorded.agent,
        handoff.as_deref(),
        recorded.session.as_deref(),
    )
    .await?;
    if !saved.commands.is_empty() && !recovery.handoff_delivered {
        eprintln!("{}", command_list(workspace, &saved.commands));
    }
    let mut records = vec![selected];
    records.extend(saved.commands.into_iter().map(|command| command.path));
    launch(ctx, workspace, recovery, records).await
}

/// Continue the agent that last exited on its own in the workspace, or forget
/// it with `discard`.
async fn continue_last_session(
    ctx: &Context,
    workspace: &Workspace,
    last: LastSession,
    discard: bool,
) -> Result<i32> {
    let path = LastSession::path(&ctx.paths, &workspace.id);
    let _claim = claim(&path)?;
    if discard {
        consume(&path)?;
        ctx.show(
            &json!({"discarded": [], "last_session": last.agent}),
            |_| outln!("Discarded the last agent session for {}", workspace.name),
        )?;
        return Ok(0);
    }
    if last.moved {
        return start_moved(ctx, workspace, &last.agent, None, vec![path]).await;
    }
    eprintln!("shoal: continuing the last {} session", last.agent);
    let recovery = Recovery::resolve(
        &ctx.paths,
        workspace,
        &last.agent,
        None,
        last.session.as_deref(),
    )
    .await?;
    launch(ctx, workspace, recovery, vec![path]).await
}

/// Start `agent` as a new agent is started, since its session is not
/// available where the workspace moved, telling it the workspace moved; it
/// consumes `records` once it has started.
async fn start_moved(
    ctx: &Context,
    workspace: &Workspace,
    agent: &str,
    handoff: Option<&str>,
    records: Vec<PathBuf>,
) -> Result<i32> {
    let launch: crate::agent::Agent = agent
        .parse()
        .with_context(|| format!("cannot start agent {agent} of the moved workspace"))?;
    eprintln!("shoal: starting a new {agent} session in the moved workspace");
    // The prompt keeps it out of desktop apps, which cannot take one.
    let options = crate::cli::agents::LaunchOptions {
        tracked: true,
        replaces: records,
    };
    crate::cli::agents::launch_agent(
        ctx,
        launch,
        workspace.clone(),
        Some(crate::execution::recovery::moved_prompt(handoff)),
        vec![],
        options,
    )
    .await
}

/// Start the restore command, consuming `records` once the agent runs.
async fn launch(
    ctx: &Context,
    workspace: &Workspace,
    recovery: Recovery,
    records: Vec<PathBuf>,
) -> Result<i32> {
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
    let settings =
        client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.id.clone())).await?;
    if let Some(code) = terminal::run_agent(
        ctx,
        &settings,
        workspace,
        &recovery.agent,
        &records,
        &recovery.command,
    )
    .await?
    {
        return Ok(code);
    }
    if let Err(error) = terminal::label_current(&workspace.id, &recovery.agent).await {
        eprintln!("warning: {error:#}");
    }
    crate::execution::run_recovery(
        &ctx.paths,
        workspace.id.clone(),
        recovery.command,
        recovery.agent,
        records,
    )
    .await
}

/// The stopped agent's execution ID and recovery record to restore: the only
/// one, or one picked on a terminal.
fn select_agent(ctx: &Context, agents: &[(String, PathBuf)]) -> Result<(String, PathBuf)> {
    match agents {
        [agent] => Ok(agent.clone()),
        agents if ctx.interactive() => {
            let labels = agents
                .iter()
                .map(|(_, path)| agent_label(path))
                .collect::<Vec<_>>();
            let choices = agents
                .iter()
                .cloned()
                .zip(labels.iter().map(String::as_str))
                .collect::<Vec<_>>();
            ui::pick_choice(ctx, "Resume agent> ", &choices)
        }
        _ => anyhow::bail!("multiple stopped agents; select one with --execution <id>"),
    }
}

/// How the agent picker names a stopped agent: its name, when it stopped,
/// and why when known.
fn agent_label(path: &Path) -> String {
    let record = Record::load(path).ok().flatten();
    let mut label = record
        .as_ref()
        .map_or_else(|| "agent".to_owned(), |record| record.agent.clone());
    let stopped = std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok());
    if let Some(stopped) = stopped {
        let time = crate::time::local_minutes(stopped.as_secs() as i64);
        label.push_str(&format!(", stopped {time}"));
    }
    if let Some(reason) = record.and_then(|record| record.stop_reason) {
        label.push_str(&format!(" after {reason}"));
    }
    label
}

/// A daemon restart quarantines interrupted executions as unknown. Recovery
/// records are still safe to use once the daemon has rechecked that execution's
/// recorded processes are gone, so make that repair part of resume itself.
async fn reconcile_stale_execution(
    ctx: &Context,
    inspection: &Inspection,
    selected: Option<&str>,
) -> Result<()> {
    let Some(selected) = selected else {
        return Ok(());
    };
    let Some(execution) = inspection
        .executions
        .iter()
        .find(|execution| execution.id == selected)
    else {
        return Ok(());
    };
    ensure!(
        execution.state != ExecutionState::Running,
        "selected execution is active; stop it before resuming"
    );
    if execution.state != ExecutionState::Unknown {
        return Ok(());
    }
    let reports = client::request::<Vec<Report>>(
        &ctx.paths,
        Method::Doctor {
            workspace: Some(inspection.workspace.id.clone()),
            options: ReconcileOptions {
                repair: true,
                // A protection handoff is persisted only after Shoal has
                // decided to stop this agent. The pause path clears the child
                // identity before the daemon can restart, so the normal
                // launch-proof acknowledgement is unavailable even though
                // process scanning still proves that no survivor is visible.
                acknowledge_stopped: true,
                ..Default::default()
            },
        },
    )
    .await?;
    let report = reports
        .into_iter()
        .next()
        .context("daemon returned no reconciliation report")?;
    ensure!(
        report
            .executions
            .iter()
            .find(|execution| execution.id == selected)
            .is_some_and(|execution| execution.cleared),
        "selected execution is still unresolved; inspect it with shoal doctor"
    );
    Ok(())
}

/// Resume each workspace's stopped work. Commands without an agent are
/// reported here. An agent resumes where it stopped when the terminal backend
/// can reuse that place; otherwise one agent resumes in this terminal, several
/// in the backend's background, and without a backend each is listed for its
/// own terminal.
async fn run_all(ctx: &Context, discard: bool) -> Result<i32> {
    let stopped = stopped_work(ctx, discard).await?;
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
            outln!("Discarded stopped agents and commands for {names}")
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
    let mut place = None;
    for (workspace, id, record) in &sessions {
        let settings =
            client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.id.clone())).await?;
        let args = ["resume", &workspace.id, "--execution", id].map(OsString::from);
        let reused = terminal::rerun_where_stopped(ctx, &settings, record, &args).await?;
        if single && reused.is_none() {
            return run(ctx, workspace.id.clone(), Some(id.clone()), false).await;
        }
        let started = match reused {
            Some(places) => Some(places),
            None => terminal::run_in_background(ctx, &settings, workspace, &args).await?,
        };
        place = started.or(place);
        let (background, reused) = (started.is_some(), reused.is_some());
        agents.push(json!({
            "workspace": workspace.name,
            "execution": id,
            "background": background,
            "reused": reused,
        }));
    }
    ctx.show(&json!({"commands": commands, "agents": agents}), |_| {
        let listed = agents
            .iter()
            .filter(|agent| agent["background"] == false)
            .count();
        if let Some(place) = place {
            let started = sessions.len() - listed;
            outln!("Resuming {started} agents in {place}");
        }
        if listed > 0 {
            outln!("Resume each agent in its own terminal:");
        }
        for ((workspace, id, _), agent) in sessions.iter().zip(&agents) {
            if agent["background"] == false {
                let words = ["shoal", "resume", &workspace.name, "--execution", id];
                outln!("  {}", crate::shell::quote(&words));
            }
        }
    })?;
    Ok(0)
}

/// The workspaces with stopped work that no waiting wrapper restores. A done
/// assignment needs no agent, so its workspace is included only to `discard`.
async fn stopped_work(ctx: &Context, discard: bool) -> Result<Vec<(Workspace, Saved)>> {
    let mut stopped = Vec::new();
    for workspace in client::workspaces(&ctx.paths).await? {
        if workspace.done && !discard {
            continue;
        }
        let mut saved = Saved::load(&ctx.paths, &workspace.id)?;
        saved.retain_unattended();
        if !saved.is_empty() {
            stopped.push((workspace, saved));
        }
    }
    Ok(stopped)
}

/// Only one resume reports a command; another holding its lock skips it.
fn claim_commands(commands: &mut Vec<SavedCommand>) -> Vec<RecoveryClaim> {
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
        outln!("{}", command_list(workspace, commands));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_are_labelled_by_name_stop_time_and_reason() {
        let root = tempfile::tempdir().unwrap();
        let record = root.path().join("execution.recovery.json");
        std::fs::write(
            &record,
            r#"{"agent":"claude","stop_reason":"critical memory pressure"}"#,
        )
        .unwrap();
        let label = agent_label(&record);
        assert!(label.starts_with("claude, stopped "), "{label}");
        assert!(
            label.ends_with(" after critical memory pressure"),
            "{label}"
        );
        assert!(!label.contains("execution"), "{label}");
    }
}
