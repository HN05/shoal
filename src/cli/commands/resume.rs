//! Manual recovery uses saved agent identity and today's restore configuration.
//! It queues restores unless asked to start them now.
use crate::{
    cli::{
        args::WorkspaceTargets, client, commands::turns::Continuation, context::Context, terminal,
        ui,
    },
    daemon::{
        recovery::{ReconcileOptions, Report},
        workspace::Discarded,
    },
    execution::recovery::{
        LastSession, Record, Recovery, RecoveryClaim, Restored, Saved, SavedCommand, claim,
        consume, saved_at,
    },
    model::{Inspection, QueueEntry, Workspace},
    protocol::{AssignmentMethod, ConfigTarget, RecoveryMethod},
    sessions::HostName,
    state::{ExecutionState, QueueState},
};
use anyhow::{Context as _, Result, ensure};
use serde_json::json;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

/// `shoal resume`: the selected workspaces, or every one for `--all` or when
/// no workspace has stopped work. Their restores queue unless `now`, or
/// `focus`, which restores one agent in this terminal.
pub async fn select(
    ctx: &Context,
    mut targets: WorkspaceTargets,
    execution: Option<String>,
    discard: bool,
    now: bool,
    focus: bool,
) -> Result<i32> {
    let scoped = crate::env::inherits_scope(&ctx.paths.state);
    if scoped {
        ensure_below(&targets, now || focus)?;
    }
    // Named workspaces whose agents idle after an interrupt continue their
    // turns instead, which a request that also resumes stopped work would
    // report apart from it.
    if !discard && execution.is_none() && !targets.workspace.is_empty() {
        let named = ui::select_workspaces(ctx, targets.workspace).await?;
        let (continuation, rest) = Continuation::plan(ctx, named).await?;
        if !continuation.is_empty() {
            ensure!(
                rest.is_empty(),
                "resume interrupted turns and stopped work separately"
            );
            return continuation.submit(ctx).await;
        }
        targets.workspace = rest;
    }
    if scoped && !discard {
        return queue_below(ctx, targets.workspace, execution).await;
    }
    let selected = if targets.all {
        None
    } else {
        ui::select_stopped_workspaces(ctx, targets.workspace).await?
    };
    if !(discard || now || focus) {
        return queue_selected(ctx, selected, execution).await;
    }
    match selected.as_deref() {
        None => run_all(ctx, None, discard).await,
        Some([workspace]) => run(ctx, workspace.clone(), execution, discard, focus).await,
        Some(workspaces) => {
            ensure!(
                execution.is_none(),
                "--execution selects a stopped execution of one workspace"
            );
            ensure!(!focus, "--focus restores the agent of one workspace");
            let workspaces = stopped_in(ctx, workspaces).await?;
            run_all(ctx, Some(workspaces), discard).await
        }
    }
}

/// Refuse a resume in a workspace process that does not name the workspaces
/// below its own or restores in this terminal, `here`: it only queues the
/// restores of the agents it stopped there, which the daemon starts, or
/// discards their stopped work.
fn ensure_below(targets: &WorkspaceTargets, here: bool) -> Result<()> {
    // The daemon checks that each workspace is below the caller's, which
    // needs the scope token: a process that cleared it stays refused.
    ensure!(
        crate::env::is_scoped() && !targets.all && !targets.workspace.is_empty() && !here,
        "workspace processes resume only the workspaces below theirs, by name and without --now or --focus: shoal resume <workspace> [--discard]"
    );
    Ok(())
}

/// Queue the restores of the `selected` workspaces' stopped agents, of
/// every workspace's for `None`.
async fn queue_selected(
    ctx: &Context,
    selected: Option<Vec<String>>,
    execution: Option<String>,
) -> Result<i32> {
    match selected.as_deref() {
        None => queue::queue_all(ctx, stopped_work(ctx, false).await?).await,
        Some([workspace]) => queue_workspace(ctx, workspace.clone(), execution).await,
        Some(workspaces) => {
            ensure!(
                execution.is_none(),
                "--execution selects a stopped execution of one workspace"
            );
            queue::queue_all(ctx, stopped_in(ctx, workspaces).await?).await
        }
    }
}

/// Queue the restores of the stopped agents and last sessions of the
/// workspaces below this process's that `selectors` name, only `execution`'s
/// when given; the daemon refuses those this process or one below it did
/// not stop, and starts the rest detached.
async fn queue_below(
    ctx: &Context,
    selectors: Vec<String>,
    execution: Option<String>,
) -> Result<i32> {
    let mut stopped = Vec::new();
    for selector in selectors {
        stopped.push(stopped_agents(ctx, selector, execution.as_deref()).await?);
    }
    queue::queue_all(ctx, stopped).await
}

/// Executions stopped so their workspaces could move, by workspace.
pub(super) type Stopped = std::collections::BTreeMap<String, std::collections::BTreeSet<String>>;

/// Queue the restores of the agents among `stopped`; their workspaces'
/// stopped commands go to them, and those of workspaces without one stay
/// for `shoal resume`.
pub(super) async fn queue_stopped(ctx: &Context, stopped: &Stopped) -> Result<queue::Queued> {
    let mut queued = Vec::new();
    for (workspace, executions) in stopped {
        let workspace = client::inspect(&ctx.paths, workspace.clone())
            .await?
            .workspace;
        let mut saved = Saved::load(&ctx.paths, &workspace.id)?;
        saved.retain(|id| executions.contains(id));
        if !(saved.agents.is_empty() && saved.sessions.is_empty()) {
            queued.push((workspace, saved));
        }
    }
    Ok(queue::queue_each(ctx, queued).await)
}

/// The stopped agents and last sessions of the workspace `selector` names,
/// only `execution`'s when given, refusing one without any. Its stopped
/// commands go to the restored agents.
async fn stopped_agents(
    ctx: &Context,
    selector: String,
    execution: Option<&str>,
) -> Result<(Workspace, Saved)> {
    let workspace = client::inspect(&ctx.paths, selector).await?.workspace;
    let mut saved = Saved::load(&ctx.paths, &workspace.id)?;
    if let Some(execution) = execution {
        saved.retain_execution(execution, true);
    }
    saved.retain_unattended();
    saved.commands.clear();
    ensure!(
        !(saved.agents.is_empty() && saved.sessions.is_empty()),
        "{} has no stopped agent to resume",
        workspace.name
    );
    Ok((workspace, saved))
}

/// Queue the restores of `workspace`'s stopped agents, only `execution`'s
/// when given. Without stopped agents, a last session continues at once, as
/// resuming the workspace by itself does not wait for a slot it never held,
/// where it stopped or in the background.
async fn queue_workspace(
    ctx: &Context,
    workspace: String,
    execution: Option<String>,
) -> Result<i32> {
    let workspace = client::inspect(&ctx.paths, workspace).await?.workspace;
    let mut saved = Saved::load(&ctx.paths, &workspace.id)?;
    if let Some(execution) = &execution {
        saved.retain_execution(execution, true);
    }
    ensure!(
        !saved.is_empty(),
        "no stopped agents or commands for this workspace"
    );
    if saved.agents.is_empty() && !saved.sessions.is_empty() {
        return continue_last_session(ctx, &workspace, saved, false).await;
    }
    queue::queue_all(ctx, vec![(workspace, saved)]).await
}

/// Restore the stopped agent `execution` of the one workspace `targets`
/// names for the daemon's start of the queued restore `entry`, whose slot the
/// agent takes over.
pub async fn start_entry(
    ctx: &Context,
    targets: WorkspaceTargets,
    execution: Option<String>,
    entry: String,
) -> Result<i32> {
    ensure!(
        !crate::env::inherits_scope(&ctx.paths.state),
        "resume agents outside scoped executions"
    );
    let ([workspace], Some(execution)) = (targets.workspace.as_slice(), execution) else {
        anyhow::bail!("a queued restore names one workspace and its stopped execution");
    };
    crate::env::launch_queue_entry(&entry);
    let _hold = super::agent_queue::Hold::start(&ctx.paths, &entry).await;
    // A stop or completion since admission cancelled it.
    let admitted = super::agent_queue::entries(ctx)
        .await?
        .iter()
        .any(|queued| queued.id == entry && queued.state == QueueState::Starting);
    ensure!(admitted, "the queued restore was cancelled");
    let restored = run(ctx, workspace.clone(), Some(execution), false, true).await;
    if let Err(error) = &restored {
        super::agent_queue::fail(ctx, &entry, error).await;
    }
    restored
}

/// The daemon's worker for the admitted restore `entry`: restore its stopped
/// agent where it stopped, otherwise in the background, through
/// [`start_entry`].
pub(super) async fn start_queued(ctx: &Context, entry: &QueueEntry) -> Result<()> {
    let (Some(workspace), Some(execution)) = (&entry.workspace_id, &entry.execution) else {
        anyhow::bail!("the queued restore names no stopped agent");
    };
    let workspace = client::inspect(&ctx.paths, workspace.clone())
        .await?
        .workspace;
    let settings =
        client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.id.clone())).await?;
    let record = crate::execution::recovery::saved_path(&ctx.paths, &workspace.id, execution);
    let args = [
        "resume",
        &workspace.id,
        "--execution",
        execution,
        "--focus",
        "--queue-entry",
        &entry.id,
    ]
    .map(OsString::from);
    terminal::restore_queued(ctx, &settings, &workspace, &record, &args).await
}

/// Record the place this restore runs in, one `host` opened in the
/// background for it, as the workspace's, before anything runs there.
pub async fn record_place(ctx: &Context, host: HostName, targets: &WorkspaceTargets) {
    if let [workspace] = targets.workspace.as_slice() {
        terminal::record_owned_here(ctx, host, workspace).await;
    }
}

/// Close the place this restore runs in, one `host` opened in the
/// background for it, once the one workspace `targets` names is done.
pub async fn close_place_when_done(ctx: &Context, host: HostName, targets: &WorkspaceTargets) {
    if let [workspace] = targets.workspace.as_slice() {
        terminal::close_background_place_when_done(ctx, host, workspace).await;
    }
}

/// Resume `workspace`'s stopped agent, the `execution` one when given, or
/// forget its stopped work for `discard`. The agent restores in this
/// terminal when `here`, otherwise where it stopped or in the background.
pub async fn run(
    ctx: &Context,
    workspace: String,
    execution: Option<String>,
    discard: bool,
    here: bool,
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
    ensure!(
        !saved.is_empty(),
        "no stopped agents or commands for this workspace"
    );
    if saved.agents.is_empty() && !saved.sessions.is_empty() {
        return continue_last_session(ctx, &inspection.workspace, saved, here).await;
    }
    let recovery_id = execution
        .as_deref()
        .or_else(|| (saved.agents.len() == 1).then(|| saved.agents[0].0.as_str()));
    reconcile_stale_execution(ctx, &inspection, recovery_id).await?;
    inspection = client::inspect(&ctx.paths, workspace_id).await?;
    let workspace = &inspection.workspace;
    let active = |id: &str| inspection.executions.iter().any(|e| e.id == id);
    if saved.agents.is_empty() {
        let _commands = claim_commands(&mut saved.commands);
        let report = report_commands(ctx, workspace, &saved.commands)?;
        ctx.show(&report, |_| {})?;
        return Ok(0);
    }
    let (selected_id, selected) = select_agent(ctx, &saved.agents, agent_label)?;
    ensure!(
        !active(&selected_id),
        "selected execution is active or unknown; stop or reconcile it first"
    );
    if !here {
        let session = (workspace.clone(), selected_id, selected);
        return start_sessions(ctx, vec![session], true, Vec::new(), Vec::new()).await;
    }
    // The restore claims the commands it reports, which a restore started
    // elsewhere claims for itself.
    let _commands = claim_commands(&mut saved.commands);
    let _claim = claim(&selected)?;
    let recorded = Record::load(&selected)?.context("recovery record was already consumed")?;
    if let Some(reason) = &recorded.stop_reason {
        eprintln!("shoal: restoring {}, stopped: {reason}", recorded.agent);
    }
    let session = recorded.into_last_session(selected_id, &selected);
    restore(ctx, workspace, session, selected, saved.commands).await
}

/// Forget the workspace's stopped work and last sessions, only
/// `execution`'s when given.
async fn discard_work(ctx: &Context, workspace: String, execution: Option<String>) -> Result<i32> {
    let discarded = client::request::<Discarded>(
        &ctx.paths,
        AssignmentMethod::DiscardStoppedWork {
            workspace,
            execution,
        },
    )
    .await?;
    ctx.show(&json!({"discarded": discarded.executions}), |_| {
        outln!(
            "Discarded stopped agents and commands for {}",
            discarded.workspace
        )
    })?;
    Ok(0)
}

/// Continue one of the workspace's last sessions that `saved` keeps, the
/// only one or one picked on a terminal, telling it about the stopped
/// commands: in this terminal when `here`, otherwise where it stopped or in
/// the background.
async fn continue_last_session(
    ctx: &Context,
    workspace: &Workspace,
    saved: Saved,
    here: bool,
) -> Result<i32> {
    let (id, path) = select_agent(ctx, &saved.sessions, session_label)?;
    if !here {
        let session = (workspace.clone(), id, path);
        return start_sessions(ctx, vec![session], true, Vec::new(), Vec::new()).await;
    }
    let _claim = claim(&path)?;
    let last = LastSession::load(&path)?.context("last session was already continued")?;
    let mut commands = saved.commands;
    let _commands = claim_commands(&mut commands);
    if !last.moved {
        eprintln!("shoal: continuing the last {} session", last.agent);
    }
    restore(ctx, workspace, last, path, commands).await
}

/// Restore the agent of `session`, saved at `path`, telling it about its stop
/// and the stopped `commands`, which the caller claimed; it consumes their
/// records once it runs.
async fn restore(
    ctx: &Context,
    workspace: &Workspace,
    session: LastSession,
    path: PathBuf,
    commands: Vec<SavedCommand>,
) -> Result<i32> {
    let handoff = handoff(&commands);
    let mut records = vec![path];
    if session.moved {
        records.extend(commands.into_iter().map(|command| command.path));
        return start_moved(ctx, workspace, &session, handoff.as_deref(), records).await;
    }
    let restored = Restored {
        agent: &session.agent,
        model: &session.model,
        session: session.session.as_deref(),
    };
    let recovery = Recovery::resolve(
        &ctx.paths,
        workspace,
        restored,
        handoff.as_deref(),
        session.stop.as_ref(),
    )
    .await?;
    if !commands.is_empty() && !recovery.handoff_delivered {
        eprintln!("{}", command_list(workspace, &commands));
    }
    records.extend(commands.into_iter().map(|command| command.path));
    launch(ctx, workspace, recovery, records).await
}

/// Start the agent of `session` as a new agent is started, with the model
/// and effort it ran with, since its session is not available where the
/// workspace moved, telling it the workspace moved; it consumes `records`
/// once it has started.
async fn start_moved(
    ctx: &Context,
    workspace: &Workspace,
    session: &LastSession,
    handoff: Option<&str>,
    records: Vec<PathBuf>,
) -> Result<i32> {
    let agent = &session.agent;
    let launch: crate::agent::Agent = agent
        .parse()
        .with_context(|| format!("cannot start agent {agent} of the moved workspace"))?;
    eprintln!("shoal: starting a new {agent} session in the moved workspace");
    // The prompt keeps it out of desktop apps, which cannot take one.
    let options = crate::cli::agents::LaunchOptions {
        tracked: true,
        replaces: records,
        model: session.model.clone(),
        ..Default::default()
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
        "configure [agents.{}] resume_command with a session restore command",
        recovery.agent
    );
    if !recovery.automatic {
        ensure!(
            ctx.interactive(),
            "session picker requires a terminal; configure [agents.{}] resume_command for noninteractive recovery",
            recovery.agent
        );
    }
    let settings =
        client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.id.clone())).await?;
    let agent = recovery.tracked();
    if let Some(code) = terminal::run_agent(
        ctx,
        &settings,
        workspace,
        &agent,
        &records,
        &recovery.command,
    )
    .await?
    {
        return Ok(code);
    }
    terminal::label_current(&ctx.paths, terminal::Owner::Workspace(&workspace.id)).await?;
    crate::execution::run_recovery(
        &ctx.paths,
        workspace.id.clone(),
        recovery.command,
        agent,
        records,
    )
    .await
}

/// The execution ID and saved file of the agent to restore of `agents`: the
/// only one, or one picked on a terminal by its `label`.
fn select_agent(
    ctx: &Context,
    agents: &[(String, PathBuf)],
    label: fn(&Path) -> String,
) -> Result<(String, PathBuf)> {
    match agents {
        [agent] => Ok(agent.clone()),
        agents if ctx.interactive() => {
            let labels = agents
                .iter()
                .map(|(_, path)| label(path))
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
    label.push_str(&stop_time(path));
    let idle = record.as_ref().is_some_and(Record::between_turns);
    if let Some(reason) = record.and_then(|record| record.stop_reason) {
        label.push_str(&format!(" ({reason})"));
    }
    label.push_str(if idle { ", idle" } else { ", interrupted" });
    label
}

/// How the agent picker names a last session: its agent and when it stopped.
fn session_label(path: &Path) -> String {
    let agent = LastSession::load(path).ok().flatten();
    let mut label = agent.map_or_else(|| "agent".to_owned(), |last| last.agent);
    label.push_str(&stop_time(path));
    label
}

/// `, stopped <time>` for the agent saved at `path`, empty when unknown.
fn stop_time(path: &Path) -> String {
    saved_at(path)
        .map(|stopped| {
            let time = crate::time::local_minutes(stopped as i64);
            format!(", stopped {time}")
        })
        .unwrap_or_default()
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
/// can reuse that place, otherwise in the backend's background; without a
/// backend one agent resumes in this terminal and several are listed for
/// their own terminals.
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
        if saved.agents.is_empty() && saved.sessions.is_empty() {
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
                    .chain(saved.sessions)
                    .map(|(id, path)| (workspace.clone(), id, path)),
            );
        }
    }
    let single = sessions.len() == 1 && ctx.interactive();
    start_sessions(ctx, sessions, single, commands, failed).await
}

/// Start each of the stopped agents `sessions` lists, by workspace,
/// execution and saved file, where it stopped or in the background; `single`
/// resumes the only one in this terminal when neither is possible. Reports
/// them with the stopped `commands` reported and the resumes already
/// `failed`.
async fn start_sessions(
    ctx: &Context,
    sessions: Vec<(Workspace, String, PathBuf)>,
    single: bool,
    commands: Vec<serde_json::Value>,
    mut failed: Vec<serde_json::Value>,
) -> Result<i32> {
    let mut agents = Vec::new();
    let mut listed = Vec::new();
    let mut place = None;
    for (workspace, id, record) in &sessions {
        let (reused, started) = match start_session(ctx, workspace, id, record, single).await {
            Ok(Started::Here) => {
                // Boxed, since `run` calls back into this function.
                let id = Some(id.clone());
                return Box::pin(run(ctx, workspace.id.clone(), id, false, true)).await;
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
            let noun = if started == 1 { "agent" } else { "agents" };
            outln!("Resuming {started} {noun} in {place}");
        }
        if !listed.is_empty() {
            outln!("Resume each agent in its own terminal:");
        }
        for (workspace, id) in &listed {
            let words = [
                "shoal",
                "resume",
                &workspace.name,
                "--execution",
                id,
                "--focus",
            ];
            outln!("  {}", crate::shell::quote(&words));
        }
    })?;
    Ok(if failed.is_empty() { 0 } else { 1 })
}

/// Where a stopped agent resumes.
enum Started {
    /// In this terminal, as the only agent to resume where the backend can
    /// start nothing elsewhere.
    Here,
    /// Where it stopped when `reused`, in the backend's background, or, when
    /// `place` is `None`, in a terminal of the user's own.
    Elsewhere {
        reused: bool,
        place: Option<&'static str>,
    },
}

/// Start the stopped agent `id` of `workspace` where it stopped or in the
/// background; when neither is possible, `single` resumes the only one in
/// this terminal.
async fn start_session(
    ctx: &Context,
    workspace: &Workspace,
    id: &str,
    record: &Path,
    single: bool,
) -> Result<Started> {
    let settings =
        client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.id.clone())).await?;
    let args = ["resume", &workspace.id, "--execution", id, "--focus"].map(OsString::from);
    if let Some(places) = terminal::rerun_where_stopped(ctx, &settings, record, &args).await? {
        return Ok(Started::Elsewhere {
            reused: true,
            place: Some(places),
        });
    }
    let place = terminal::run_in_background(ctx, &settings, workspace, &args).await?;
    if single && place.is_none() {
        return Ok(Started::Here);
    }
    Ok(Started::Elsewhere {
        reused: false,
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

/// The stopped work and last sessions of the workspaces `selectors` name,
/// refusing one with nothing to resume.
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
            "{} has no stopped agents or commands to resume",
            workspace.name
        );
        stopped.push((workspace, saved));
    }
    Ok(stopped)
}

/// The workspaces with stopped work or last sessions that no waiting wrapper
/// restores. A done assignment needs no agent, so its workspace is included
/// only to `discard`.
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

mod queue;
pub(super) use queue::Queued;

#[cfg(test)]
mod tests;
