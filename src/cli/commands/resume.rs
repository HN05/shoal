//! Manual recovery uses saved agent identity and today's restore configuration.
use crate::{
    cli::{args::WorkspaceTargets, client, context::Context, terminal, ui},
    daemon::{
        recovery::{ReconcileOptions, Report},
        workspace::Discarded,
    },
    execution::recovery::{
        LastSession, Record, Recovery, RecoveryClaim, Saved, SavedCommand, claim, consume,
    },
    model::{Inspection, Workspace},
    protocol::{AssignmentMethod, ConfigTarget, RecoveryMethod},
    state::ExecutionState,
};
use anyhow::{Context as _, Result, ensure};
use serde_json::json;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

/// `shoal resume`: the selected workspaces, or every one for `--all` or when
/// no workspace has stopped work.
pub async fn select(
    ctx: &Context,
    targets: WorkspaceTargets,
    execution: Option<String>,
    discard: bool,
) -> Result<i32> {
    // The daemon checks that the workspace is below the caller's, which
    // needs the scope token: a process that cleared it stays refused.
    let checked_discard = crate::env::is_scoped() && discard && targets.workspace.len() == 1;
    ensure!(
        !crate::env::inherits_scope(&ctx.paths.state) || checked_discard,
        "resume agents outside scoped executions; workspace processes can only discard the stopped work of a workspace below theirs with shoal resume <workspace> --discard"
    );
    let selected = if targets.all {
        None
    } else {
        ui::select_stopped_workspaces(ctx, targets.workspace).await?
    };
    match selected.as_deref() {
        None => run_all(ctx, None, discard).await,
        Some([workspace]) => run(ctx, workspace.clone(), execution, discard).await,
        Some(workspaces) => {
            ensure!(
                execution.is_none(),
                "--execution selects a stopped execution of one workspace"
            );
            let workspaces = stopped_in(ctx, workspaces).await?;
            run_all(ctx, Some(workspaces), discard).await
        }
    }
}

pub async fn run(
    ctx: &Context,
    workspace: String,
    execution: Option<String>,
    discard: bool,
) -> Result<i32> {
    if discard {
        return discard_work(ctx, workspace, execution).await;
    }
    let mut inspection = client::inspect(&ctx.paths, workspace).await?;
    let workspace_id = inspection.workspace.id.clone();
    let mut saved = Saved::load(&ctx.paths, &workspace_id)?;
    if let Some(execution) = &execution {
        saved.retain_execution(execution, true);
    }
    if saved.is_empty()
        && execution.is_none()
        && let Some(last) = LastSession::load(&ctx.paths, &workspace_id)?
    {
        return continue_last_session(ctx, &inspection.workspace, last).await;
    }
    ensure!(
        !saved.is_empty(),
        "no stopped agents or commands for this workspace"
    );
    let recovery_id = execution
        .as_deref()
        .or_else(|| (saved.agents.len() == 1).then(|| saved.agents[0].0.as_str()));
    reconcile_stale_execution(ctx, &inspection, recovery_id).await?;
    inspection = client::inspect(&ctx.paths, workspace_id).await?;
    let workspace = &inspection.workspace;
    let active = |id: &str| inspection.executions.iter().any(|e| e.id == id);
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

/// Forget the workspace's stopped work, only `execution`'s when given, or
/// else its last session.
async fn discard_work(ctx: &Context, workspace: String, execution: Option<String>) -> Result<i32> {
    let discarded = client::request::<Discarded>(
        &ctx.paths,
        AssignmentMethod::DiscardStoppedWork {
            workspace,
            execution,
        },
    )
    .await?;
    match &discarded.last_session {
        Some(agent) => ctx.show(&json!({"discarded": [], "last_session": agent}), |_| {
            outln!(
                "Discarded the last agent session for {}",
                discarded.workspace
            )
        })?,
        None => ctx.show(&json!({"discarded": discarded.executions}), |_| {
            outln!(
                "Discarded stopped agents and commands for {}",
                discarded.workspace
            )
        })?,
    }
    Ok(0)
}

/// Continue the agent that last exited on its own in the workspace.
async fn continue_last_session(
    ctx: &Context,
    workspace: &Workspace,
    last: LastSession,
) -> Result<i32> {
    let path = LastSession::path(&ctx.paths, &workspace.id);
    let _claim = claim(&path)?;
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
/// why when known, and whether it was interrupted mid-turn or idle.
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
    let idle = record.as_ref().is_some_and(Record::between_turns);
    if let Some(reason) = record.and_then(|record| record.stop_reason) {
        label.push_str(&format!(" after {reason}"));
    }
    label.push_str(if idle { ", idle" } else { ", interrupted" });
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
        RecoveryMethod::Reconcile {
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
        "selected execution is still unresolved; stop what remains of it with shoal stop, then resume again"
    );
    Ok(())
}

/// Resume each workspace's stopped work. Commands without an agent are
/// reported here. An agent resumes where it stopped when the terminal backend
/// can reuse that place; otherwise one agent resumes in this terminal, several
/// in the backend's background, and without a backend each is listed for its
/// own terminal.
async fn run_all(
    ctx: &Context,
    selected: Option<Vec<(Workspace, Saved)>>,
    discard: bool,
) -> Result<i32> {
    let stopped = match selected {
        Some(stopped) => stopped,
        None => stopped_work(ctx, discard).await?,
    };
    if stopped.is_empty() {
        ctx.emit(
            "No stopped agents or commands",
            json!({"commands": [], "agents": []}),
        )?;
        return Ok(0);
    }
    if discard {
        return discard_each(ctx, &stopped).await;
    }
    let mut failed = Vec::new();
    let mut commands = Vec::new();
    let mut sessions = Vec::new();
    for (workspace, mut saved) in stopped {
        if saved.agents.is_empty() {
            let _claims = claim_commands(&mut saved.commands);
            match report_commands(ctx, &workspace, &saved.commands) {
                Ok(report) => commands.push(report),
                Err(error) => failed.push(ui::failure(ctx, "resume", &workspace.name, &error)),
            }
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
    let mut listed = Vec::new();
    let mut place = None;
    for (workspace, id, record) in &sessions {
        let (reused, started) = match start_session(ctx, workspace, id, record, single).await {
            Ok(Started::Here) => {
                return run(ctx, workspace.id.clone(), Some(id.clone()), false).await;
            }
            Ok(Started::Elsewhere { reused, place }) => (reused, place),
            Err(error) => {
                failed.push(ui::failure(ctx, "resume", &workspace.name, &error));
                continue;
            }
        };
        place = started.or(place);
        if started.is_none() {
            listed.push((workspace, id));
        }
        agents.push(json!({
            "workspace": workspace.name,
            "execution": id,
            "background": started.is_some(),
            "reused": reused,
        }));
    }
    let output = json!({"commands": commands, "agents": agents, "failed": failed});
    ctx.show(&output, |_| {
        if let Some(place) = place {
            let started = agents.len() - listed.len();
            outln!("Resuming {started} agents in {place}");
        }
        if !listed.is_empty() {
            outln!("Resume each agent in its own terminal:");
        }
        for (workspace, id) in &listed {
            let words = ["shoal", "resume", &workspace.name, "--execution", id];
            outln!("  {}", crate::shell::quote(&words));
        }
    })?;
    Ok(if failed.is_empty() { 0 } else { 1 })
}

/// Where a stopped agent resumes.
enum Started {
    /// In this terminal, as the only agent to resume.
    Here,
    /// Where it stopped when `reused`, in the backend's background, or, when
    /// `place` is `None`, in a terminal of the user's own.
    Elsewhere {
        reused: bool,
        place: Option<&'static str>,
    },
}

/// Start the stopped agent `id` of `workspace` where it stopped or in the
/// background; `single` resumes the only one in this terminal instead.
async fn start_session(
    ctx: &Context,
    workspace: &Workspace,
    id: &str,
    record: &Path,
    single: bool,
) -> Result<Started> {
    let settings =
        client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.id.clone())).await?;
    let args = ["resume", &workspace.id, "--execution", id].map(OsString::from);
    let reused = terminal::rerun_where_stopped(ctx, &settings, record, &args).await?;
    if single && reused.is_none() {
        return Ok(Started::Here);
    }
    let place = match reused {
        Some(places) => Some(places),
        None => terminal::run_in_background(ctx, &settings, workspace, &args).await?,
    };
    Ok(Started::Elsewhere {
        reused: reused.is_some(),
        place,
    })
}

/// Forget every workspace's stopped agents and commands, going on after one
/// fails.
async fn discard_each(ctx: &Context, stopped: &[(Workspace, Saved)]) -> Result<i32> {
    let mut ids = Vec::new();
    let mut names = Vec::new();
    let mut failed = Vec::new();
    for (workspace, saved) in stopped {
        match discard_saved(ctx, workspace, saved).await {
            Ok(executions) => {
                ids.extend(executions);
                names.push(workspace.name.as_str());
            }
            Err(error) => failed.push(ui::failure(ctx, "discard", &workspace.name, &error)),
        }
    }
    ctx.show(&json!({"discarded": ids, "failed": failed}), |_| {
        if !names.is_empty() {
            outln!(
                "Discarded stopped agents and commands for {}",
                names.join(", ")
            );
        }
    })?;
    Ok(if failed.is_empty() { 0 } else { 1 })
}

/// Forget `workspace`'s stopped records each by itself, leaving agents a
/// wrapper waits to restore, and return the executions forgotten.
async fn discard_saved(ctx: &Context, workspace: &Workspace, saved: &Saved) -> Result<Vec<String>> {
    let mut ids = Vec::new();
    for execution in saved.execution_ids() {
        let method = AssignmentMethod::DiscardStoppedWork {
            workspace: workspace.id.clone(),
            execution: Some(execution),
        };
        let discarded = client::request::<Discarded>(&ctx.paths, method).await?;
        ids.extend(discarded.executions);
    }
    Ok(ids)
}

/// The stopped work of the workspaces `selectors` name, refusing one with
/// nothing stopped: only resuming it by itself continues its last session.
async fn stopped_in(ctx: &Context, selectors: &[String]) -> Result<Vec<(Workspace, Saved)>> {
    let mut stopped = Vec::new();
    for selector in selectors {
        let workspace = client::inspect(&ctx.paths, selector.clone())
            .await?
            .workspace;
        let mut saved = Saved::load(&ctx.paths, &workspace.id)?;
        saved.retain_unattended();
        ensure!(
            !saved.is_empty(),
            "{} has no stopped agents or commands to resume; resume it by itself to continue its last session",
            workspace.name
        );
        stopped.push((workspace, saved));
    }
    Ok(stopped)
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
        let mut words = vec!["shoal", "run", "--workspace", &workspace.name, "--"];
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
mod tests;
