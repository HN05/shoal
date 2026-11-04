//! How text shows a space's status: a workspace's adds what needs its
//! worktree.
use super::StatusView;
use crate::{
    cli::{
        commands::{base, ci, links, resources, split},
        list_status::ListStatus,
        output::{Palette, Style},
        ui,
    },
    daemon::links::pr::RegistrationKind,
    forge::review::status::PrStatus,
    model::{Chat, SpaceDetail, SpaceStatus, WorktreeStatus},
};

pub(super) fn render_status(view: &StatusView, json: bool) {
    let palette = Palette::stdout(json);
    let status = &view.status;
    match &status.space {
        SpaceDetail::Workspace(worktree) => render_workspace(worktree, palette),
        SpaceDetail::Chat { chat } => render_chat(chat, palette),
    }
    render_activity(view, palette);
    if let Some(worktree) = status.space.worktree() {
        render_leases(worktree, palette);
    }
    render_assignment(status, palette);
    if let Some(worktree) = status.space.worktree() {
        render_cleanup(worktree, palette);
    }
    render_attention(status);
}

/// The workspace, its branch, setup, changes and holds.
fn render_workspace(status: &WorktreeStatus, palette: Palette) {
    let workspace = &status.workspace;
    outln!(
        "{}  {}",
        palette.paint(Style::Heading, &workspace.name),
        palette.workspace_state(workspace.state)
    );
    outln!("Path:          {}", workspace.path.display());
    match (&workspace.branch, &workspace.at) {
        (Some(branch), _) => outln!("Branch:        {branch}"),
        (None, Some(at)) => outln!("At:            {}", super::super::commit_label(at)),
        (None, None) => {}
    }
    base::render(workspace);
    split::render(&workspace.parent, &workspace.children);
    if !status.chats.is_empty() {
        let chats: Vec<_> = status.chats.iter().map(|chat| chat.name.as_str()).collect();
        outln!("Chats:         {}", chats.join(", "));
    }
    outln!(
        "Setup:         {}",
        match (workspace.setup_skipped, status.setup_finished) {
            (true, _) => "skipped",
            (false, true) => "finished",
            (false, false) => "in progress",
        }
    );
    if workspace.disposable {
        outln!("Disposable:    yes");
    }
    if let Some(job) = &workspace.job {
        outln!("Job:           {}", ui::job_subject(job));
        outln!("Run:           {}", job.run.url);
    }
    match &status.diff {
        Some(diff) => outln!(
            "Changes:       {} file{}, +{} -{}",
            diff.files_changed,
            if diff.files_changed == 1 { "" } else { "s" },
            diff.insertions,
            diff.deletions
        ),
        None => {
            outln!("Changes:       unavailable");
            if let Some(error) = &status.diff_error {
                outln!("  {}", palette.paint(Style::Warning, error));
            }
        }
    }
    render_changed_on_both_sides(status, palette);
    render_sync(status, palette);
    if let Some(error) = &workspace.error {
        outln!("Error:         {}", palette.paint(Style::Error, error));
    }
    outln!("Holds:         {}", workspace.holds.len());
    for hold in &workspace.holds {
        outln!(
            "  {}{}",
            hold.name,
            hold.reason
                .as_deref()
                .map_or(String::new(), |r| format!(" ({r})"))
        );
    }
}

/// The sensitive paths the last conflict check found changed on both sides.
fn render_changed_on_both_sides(status: &WorktreeStatus, palette: Palette) {
    let changed = status.changed_on_both_sides.as_ref();
    let Some(changed) = changed.filter(|changed| !changed.files.is_empty()) else {
        return;
    };
    let line = format!(
        "Changed on both sides with {}: {}",
        changed.target,
        changed.files.join(", ")
    );
    outln!("  {}", palette.paint(Style::Warning, &line));
}

/// The chat, its status as listings show it, its agent, and the spaces
/// around it.
fn render_chat(chat: &Chat, palette: Palette) {
    let status = ListStatus::of_chat(chat);
    outln!(
        "{}  {}",
        palette.paint(Style::Heading, &chat.name),
        palette.paint(status.style(), status.label())
    );
    outln!("Agent:         {}", chat.agent);
    if let Some(workspace) = &chat.workspace {
        outln!("Workspace:     {}", workspace.name);
    }
    split::render(&chat.parent, &chat.children);
}

/// When the repository's default branch last synced with its remote, and
/// why the syncs since then failed.
fn render_sync(status: &WorktreeStatus, palette: Palette) {
    let Some(sync) = &status.sync else {
        return;
    };
    let now = crate::time::unix_seconds() as i64;
    let at = |unix: u64| crate::time::local_ago(unix as i64, now);
    outln!(
        "Last sync:     {}",
        sync.synced_at.map_or_else(|| "none".to_owned(), at)
    );
    if let (Some(error), Some(since)) = (&sync.error, sync.failing_since) {
        let error = error.lines().next().unwrap_or_default();
        let failing = format!("failing since {}: {error}", at(since));
        outln!("  {}", palette.paint(Style::Warning, &failing));
    }
}

/// What runs in the space: a workspace's executions and agent, and the
/// sessions of either.
fn render_activity(view: &StatusView, palette: Palette) {
    if let Some(worktree) = view.status.space.worktree() {
        render_processes(worktree, palette);
    }
    render_sessions(view, palette);
    render_transcripts(view, palette);
    if let Some(worktree) = view.status.space.worktree() {
        render_agent(worktree);
    }
}

/// The agent sessions found for the space, or why they could not be listed.
fn render_sessions(view: &StatusView, palette: Palette) {
    outln!("Sessions:      {}", view.sessions.len());
    for session in &view.sessions {
        outln!("  {}", session.label());
    }
    if let Some(error) = &view.sessions_error {
        outln!("  {}", palette.paint(Style::Warning, error));
    }
}

/// Each session the space's agents reported, with its transcript's path or
/// why it has none here.
fn render_transcripts(view: &StatusView, palette: Palette) {
    outln!("Transcripts:   {}", view.transcripts.len());
    for transcript in &view.transcripts {
        let session = &transcript.session;
        let running = if session.current { "  running" } else { "" };
        let path = match (&transcript.path, &transcript.error) {
            (Some(path), _) => path.display().to_string(),
            (None, Some(error)) => palette.paint(Style::Warning, error),
            (None, None) => "not found".to_owned(),
        };
        outln!("  {}  {}{running}  {path}", session.agent, session.session);
    }
    if let Some(error) = &view.transcripts_error {
        outln!("  {}", palette.paint(Style::Warning, error));
    }
}

/// The workspace's executions, what its processes use, and those left over.
fn render_processes(status: &WorktreeStatus, palette: Palette) {
    outln!("Executions:    {}", status.executions.len());
    for execution in &status.executions {
        let pid = execution
            .child
            .as_ref()
            .or(execution.wrapper.as_ref())
            .map(|identity| format!("  pid {}", identity.pid))
            .unwrap_or_default();
        outln!(
            "  {}  {}{}",
            execution.id,
            palette.execution_state(execution.state),
            pid
        );
    }

    match &status.workspace.usage {
        Some(usage) => outln!(
            "Usage:         {}, {} process{}",
            ui::usage_label(usage),
            usage.processes,
            if usage.processes == 1 { "" } else { "es" }
        ),
        None => outln!("Usage:         none"),
    }
    render_jobs(status, palette);
    outln!("Leftovers:     {}", status.leftover_count);
    for leftover in &status.leftovers {
        outln!("  {}  {}", leftover.pid, leftover.command);
    }
    if let Some(more) = status.leftover_count.checked_sub(status.leftovers.len())
        && more > 0
    {
        outln!("  and {more} more");
    }
}

/// The CI jobs running for the workspace's PR or branch, each with what its
/// processes use.
fn render_jobs(status: &WorktreeStatus, palette: Palette) {
    if status.jobs.is_empty() {
        return;
    }
    outln!("CI jobs:       {}", status.jobs.len());
    for job in &status.jobs {
        let subject = job.job.as_deref().map(ui::job_subject).unwrap_or_default();
        let usage = job.usage.as_ref().map(ui::usage_label).unwrap_or_default();
        outln!(
            "  {}  {subject}  {}",
            palette.paint(Style::Heading, &job.name),
            palette.paint(Style::Muted, &usage)
        );
    }
}

/// The turn state the workspace's agent reported and its retries.
fn render_agent(status: &WorktreeStatus) {
    let agent_state = status.workspace.agent_state;
    outln!(
        "Agent state:   {}",
        agent_state.map_or("none", |status| { ui::agent_state_label(status.state) })
    );
    if let Some(expires_at) = agent_state.and_then(|status| status.prompt_cache_expires_at) {
        outln!("Prompt cache:  {}", prompt_cache_label(expires_at));
    }
    if let Some(retry) = &status.proxy_retry {
        outln!("API retry:     {}", proxy_retry_label(retry));
    }
    if let Some(retry) = &status.turn_retry {
        outln!("Turn retry:    {}", turn_retry_label(retry));
    }
}

/// The ports, simulators and resources the workspace holds.
fn render_leases(status: &WorktreeStatus, palette: Palette) {
    outln!("Ports:         {}", status.ports.len());
    for port in &status.ports {
        outln!("  {}={} ({})", port.name, port.port, port.env_var);
    }

    outln!("Simulators:    {}", status.simulators.len());
    for simulator in &status.simulators {
        outln!(
            "  {}={}  {}  {}  {}",
            simulator.lease_name.as_deref().unwrap_or("default"),
            simulator.udid.as_deref().unwrap_or("pending"),
            palette.simulator_state(simulator.state),
            simulator.device,
            simulator.runtime
        );
    }

    outln!("Resources:     {}", status.resources.len());
    for resource in &status.resources {
        outln!(
            "  {}/{} -> {} [{}]{}",
            resource.pool,
            resource.name,
            resource.resource,
            resource.mode,
            resources::repository_path(resource)
        );
    }
}

/// The space's issue, completion and PRs.
fn render_assignment(status: &SpaceStatus, palette: Palette) {
    if let Some(issue) = &status.issue {
        outln!("Issue:         {}", issue.url);
        if let Some(state) = status.issue_status.as_ref().and_then(|s| s.state.as_ref()) {
            outln!("  State:       {state}");
        }
        let lookup = status.issue_status.as_ref().and_then(|s| s.error.as_ref());
        for error in issue.error.iter().chain(lookup) {
            outln!("  {}", palette.paint(Style::Warning, error));
        }
    }
    if let Some(completion) = &status.completion {
        let choice = if completion.cleanup {
            "cleanup requested"
        } else {
            "no cleanup requested"
        };
        let integrated = if completion.integrated {
            ", integrated elsewhere"
        } else {
            ""
        };
        outln!("Completion:    done ({choice}{integrated})");
        if !completion.results.is_empty() {
            let names: Vec<_> = completion
                .results
                .iter()
                .map(|result| result.name.as_str())
                .collect();
            outln!("  Results:     {}", names.join(", "));
        }
        if let Some(error) = &completion.error {
            outln!("  {}", palette.paint(Style::Warning, error));
        }
    }
    render_pr_watch(status, palette);
    let links = status.space.links();
    render_recorded("Opened:", &links.opened);
    render_recorded("Related:", &links.related);
}

fn render_pr_watch(status: &SpaceStatus, palette: Palette) {
    match &status.pr_cleanup {
        Some(registration) => {
            match &registration.kind {
                RegistrationKind::Watch { urls, .. } => {
                    for url in urls {
                        outln!("PR watch:      {url}");
                        if let Some(pr) = status.prs.iter().find(|pr| &pr.url == url) {
                            render_pr(pr, palette);
                        }
                    }
                }
                RegistrationKind::Acknowledgement { head } => outln!("PR watch:      {head}"),
            }
            if let Some(error) = &registration.error {
                outln!("  {}", palette.paint(Style::Warning, error));
            }
        }
        None => outln!("PR watch:      none"),
    }
}

/// When and why cleanup removes the workspace.
fn render_cleanup(status: &WorktreeStatus, palette: Palette) {
    outln!("Cleanup:       {}", status.cleanup_reason);
    if let Some(at) = status.cleanup_at {
        outln!(
            "Grace period:  ends {}",
            crate::time::local_minutes(at as i64)
        );
    }
    if let Some(error) = &status.cleanup_error {
        outln!("Auto cleanup:  {}", palette.paint(Style::Warning, error));
    }
}

/// What awaits the user: the review mark, questions, approval requests,
/// attached files and unread notifications.
fn render_attention(status: &SpaceStatus) {
    let review = status.space.review();
    if review.is_empty() {
        outln!("Review:        none");
    } else {
        outln!("Review:        ready for review");
        outln!("{}", links::review_lines(review, status.space.kind()));
    }
    let attention = &status.attention;
    outln!("Questions:     {} open", attention.open_questions);
    outln!("Approvals:     {} pending", attention.pending_approvals);
    outln!("Files:         {} attached", attention.attached_files);
    outln!("Notifications: {} unread", attention.unread_notifications);
}

fn render_recorded(label: &str, items: &[crate::model::LinkedItem]) {
    for item in items {
        match &item.title {
            Some(title) => outln!("{label:<15}{} {title}", item.url),
            None => outln!("{label:<15}{}", item.url),
        }
    }
}

fn render_pr(pr: &PrStatus, palette: Palette) {
    if let Some(state) = pr.state {
        let conflicts = match pr.merge_conflicts {
            Some(true) => format!(", {}", palette.paint(Style::Error, "merge conflicts")),
            Some(false) => ", no merge conflicts".into(),
            None => String::new(),
        };
        outln!("  State:       {state}{conflicts}");
    }
    if let Some(review) = pr.review {
        outln!("  Review:      {}", palette.review_state(review));
    }
    if pr.state.is_some() {
        outln!("  Checks:      {}", pr.checks.len());
    }
    for check in &pr.checks {
        outln!(
            "    {}: {}",
            check.name,
            palette.check_result(&check.result)
        );
    }
    if !pr.waiting_runs.is_empty() {
        let line = ci::waiting_runs_line(&pr.url, pr.waiting_runs.len());
        outln!("  {}", palette.paint(Style::Warning, &line));
    }
    for error in &pr.errors {
        outln!("  {}", palette.paint(Style::Warning, error));
    }
}

/// A request the agent's proxy waits to retry as `status` shows it.
fn proxy_retry_label(retry: &crate::daemon::workspace::proxy_retries::ProxyRetry) -> String {
    let now = i64::try_from(crate::time::unix_seconds()).unwrap_or(i64::MAX);
    format!(
        "{} (attempt {} failed), next attempt in {}s",
        retry.failure.describe(),
        retry.attempt,
        retry.next_at.saturating_sub(now).max(0)
    )
}

/// When the idle agent's prompt cache expires, or expired, as `status`
/// shows it.
fn prompt_cache_label(expires_at: i64) -> String {
    let now = i64::try_from(crate::time::unix_seconds()).unwrap_or(i64::MAX);
    let at = crate::time::local_minutes(expires_at);
    if expires_at > now {
        format!("expires at {at}")
    } else {
        format!("expired at {at}")
    }
}

/// A pending retry of the agent's failed turn as `status` shows it.
fn turn_retry_label(retry: &crate::daemon::workspace::turn_retry::TurnRetry) -> String {
    let (failure, attempt) = (retry.failure.describe(), retry.attempt);
    if retry.delivered {
        format!("{failure} (attempt {attempt}), agent asked to continue")
    } else {
        let due = crate::time::local_minutes(retry.due_at);
        format!("{failure} (attempt {attempt}) at {due}")
    }
}
