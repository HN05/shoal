//! How text shows a workspace's status.
use super::StatusView;
use crate::{
    cli::{
        commands::{base, links, resources, split},
        output::{Palette, Style},
        ui,
    },
    daemon::links::pr::RegistrationKind,
    forge::review::status::PrStatus,
    model::{Inspection, WorkspaceStatus},
};

pub(super) fn render_status(view: &StatusView, json: bool) {
    let palette = Palette::stdout(json);
    let status = &view.status;
    render_workspace(status, palette);
    render_activity(view, palette);
    render_leases(&status.inspection, palette);
    render_assignment(status, palette);
    render_cleanup(status, palette);
    render_attention(status);
}

/// The workspace, its branch, setup, changes and holds.
fn render_workspace(status: &WorkspaceStatus, palette: Palette) {
    let inspection = &status.inspection;
    let workspace = &inspection.workspace;
    outln!(
        "{}  {}",
        palette.paint(Style::Heading, &workspace.name),
        palette.workspace_state(workspace.state)
    );
    outln!("Path:          {}", workspace.path.display());
    outln!("Branch:        {}", workspace.branch);
    base::render(workspace);
    split::render(workspace);
    outln!(
        "Setup:         {}",
        if status.setup_finished {
            "finished"
        } else {
            "in progress"
        }
    );
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
    if let Some(error) = &workspace.error {
        outln!("Error:         {}", palette.paint(Style::Error, error));
    }
    outln!("Holds:         {}", inspection.workspace.holds.len());
    for hold in &inspection.workspace.holds {
        outln!(
            "  {}{}",
            hold.name,
            hold.reason
                .as_deref()
                .map_or(String::new(), |r| format!(" ({r})"))
        );
    }
}

/// What runs in the workspace: its executions, sessions and agent.
fn render_activity(view: &StatusView, palette: Palette) {
    let status = &view.status;
    let inspection = &status.inspection;
    let workspace = &inspection.workspace;
    outln!("Executions:    {}", inspection.executions.len());
    for execution in &inspection.executions {
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

    match &workspace.usage {
        Some(usage) => outln!(
            "Usage:         {}, {} process{}",
            ui::usage_label(usage),
            usage.processes,
            if usage.processes == 1 { "" } else { "es" }
        ),
        None => outln!("Usage:         none"),
    }

    outln!("Sessions:      {}", view.sessions.len());
    for session in &view.sessions {
        outln!("  {}", session.label());
    }
    if let Some(error) = &view.sessions_error {
        outln!("  {}", palette.paint(Style::Warning, error));
    }

    outln!(
        "Agent state:   {}",
        workspace
            .agent_state
            .map_or("none", |status| { ui::agent_state_label(status.state) })
    );
    if let Some(expires_at) = workspace
        .agent_state
        .and_then(|status| status.prompt_cache_expires_at)
    {
        outln!("Prompt cache:  {}", prompt_cache_label(expires_at));
    }
    if let Some(retry) = &status.proxy_retry {
        outln!("API retry:     {}", proxy_retry_label(retry));
    }
    if let Some(retry) = &status.turn_retry {
        outln!("Turn retry:    {}", turn_retry_label(retry));
    }
    outln!("Chat:          {}", view.chat.as_deref().unwrap_or("none"));
}

/// The ports, simulators and resources the workspace holds.
fn render_leases(inspection: &Inspection, palette: Palette) {
    outln!("Ports:         {}", inspection.ports.len());
    for port in &inspection.ports {
        outln!("  {}={} ({})", port.name, port.port, port.env_var);
    }

    outln!("Simulators:    {}", inspection.simulators.len());
    for simulator in &inspection.simulators {
        outln!(
            "  {}={}  {}  {}  {}",
            simulator.lease_name.as_deref().unwrap_or("default"),
            simulator.udid.as_deref().unwrap_or("pending"),
            palette.simulator_state(simulator.state),
            simulator.device,
            simulator.runtime
        );
    }

    outln!("Resources:     {}", inspection.resources.len());
    for resource in &inspection.resources {
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

/// The workspace's issue, completion and PRs.
fn render_assignment(status: &WorkspaceStatus, palette: Palette) {
    let inspection = &status.inspection;
    let workspace = &inspection.workspace;
    if let Some(issue) = &inspection.issue {
        outln!("Issue:         {}", issue.url);
        if let Some(state) = status.issue_status.as_ref().and_then(|s| s.state.as_ref()) {
            outln!("  State:       {state}");
        }
        let lookup = status.issue_status.as_ref().and_then(|s| s.error.as_ref());
        for error in issue.error.iter().chain(lookup) {
            outln!("  {}", palette.paint(Style::Warning, error));
        }
    }
    if let Some(completion) = &inspection.completion {
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
    render_recorded("Opened:", &workspace.links.opened);
    render_recorded("Related:", &workspace.links.related);
}

fn render_pr_watch(status: &WorkspaceStatus, palette: Palette) {
    match &status.inspection.pr_cleanup {
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
fn render_cleanup(status: &WorkspaceStatus, palette: Palette) {
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

/// What awaits the user: the review mark and unread notifications.
fn render_attention(status: &WorkspaceStatus) {
    let workspace = &status.inspection.workspace;
    if workspace.review.is_empty() {
        outln!("Review:        none");
    } else {
        outln!("Review:        ready for review");
        outln!("{}", links::review_lines(&workspace.review));
    }
    outln!("Notifications: {} unread", status.unread_notifications);
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
