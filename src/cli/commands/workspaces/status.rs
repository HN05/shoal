//! A workspace's status and how text shows it.
use anyhow::{Context as _, Result, ensure};

use crate::{
    cli::{
        client::{self, request},
        commands::{base, links, resources, split},
        context::Context,
        output::{Palette, Style},
        terminal,
        ui::{self, Fallback},
    },
    daemon::links::pr::RegistrationKind,
    forge::{IssueInput, items::link::Selection, review::status::PrStatus},
    model::{Chat, Workspace, WorkspaceStatus, WorkspaceTarget},
    protocol::WorkspaceMethod,
};

/// A pending retry of the agent's failed turn as `status` shows it.
fn turn_retry_label(retry: &crate::daemon::workspace::turn_retry::TurnRetry) -> String {
    let progress = match retry.attempts {
        Some(limit) => format!("attempt {} of {limit}", retry.attempt),
        None => format!("attempt {}", retry.attempt),
    };
    if retry.delivered {
        format!(
            "{} ({progress}), agent asked to continue",
            retry.failure.describe()
        )
    } else {
        format!(
            "{} ({progress}) at {}",
            retry.failure.describe(),
            crate::time::local_minutes(retry.due_at)
        )
    }
}

pub(in crate::cli::commands) async fn status(
    ctx: &Context,
    workspace: Option<String>,
    item: Option<String>,
) -> Result<i32> {
    let Some(target) = status_target(workspace.as_deref(), item)? else {
        let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
        let status = StatusView::request(ctx, workspace).await?;
        ctx.show(&status, |status| render_status(status, ctx.json))?;
        return Ok(0);
    };
    let workspaces =
        request::<Vec<Workspace>>(&ctx.paths, WorkspaceMethod::FindWorkspaces { target }).await?;
    let mut statuses = Vec::new();
    for workspace in workspaces {
        statuses.push(StatusView::request(ctx, workspace.id).await?);
    }
    ctx.show(&statuses, |statuses| {
        for (index, status) in statuses.iter().enumerate() {
            if index > 0 {
                outln!();
            }
            render_status(status, ctx.json);
        }
    })?;
    Ok(0)
}

/// The daemon's workspace status with the agent sessions the CLI finds.
#[derive(serde::Serialize)]
struct StatusView {
    #[serde(flatten)]
    status: WorkspaceStatus,
    sessions: Vec<terminal::Session>,
    /// Why sessions could not be listed; status still reports the rest.
    #[serde(skip_serializing_if = "Option::is_none")]
    sessions_error: Option<String>,
    /// How text names the workspace's chat; JSON gives its ID.
    #[serde(skip)]
    chat: Option<String>,
}

impl StatusView {
    async fn request(ctx: &Context, workspace: String) -> Result<Self> {
        // Listing chats first ends finished ones, returning their workspaces to the user.
        let chats = client::chats(&ctx.paths).await?;
        let status =
            request::<WorkspaceStatus>(&ctx.paths, WorkspaceMethod::WorkspaceStatus { workspace })
                .await?;
        let chat = chat_name(&chats, &status.inspection.workspace);
        let (sessions, sessions_error) =
            match terminal::sessions(terminal::Owner::Workspace(&status.inspection.workspace.id))
                .await
            {
                Ok(sessions) => (sessions, None),
                Err(error) => (Vec::new(), Some(format!("{error:#}"))),
            };
        Ok(Self {
            status,
            sessions,
            sessions_error,
            chat,
        })
    }
}

/// The listed chat that opened `workspace` with its agent, else its ID, which
/// scoped callers see without the chat.
fn chat_name(chats: &[Chat], workspace: &Workspace) -> Option<String> {
    let id = workspace.chat.as_ref()?;
    Some(match chats.iter().find(|chat| chat.id == *id) {
        Some(chat) => format!("{} ({})", chat.name, chat.agent),
        None => id.clone(),
    })
}

/// Items and resources find the workspaces that link or hold them; anything
/// else selects a workspace.
fn status_target(first: Option<&str>, item: Option<String>) -> Result<Option<WorkspaceTarget>> {
    match first {
        Some("resource") => Ok(Some(WorkspaceTarget::Resource {
            name: item.context("resource needs a pool or member name")?,
        })),
        Some(first)
            if matches!(first, "pr" | "issue") || IssueInput::parse(first) == IssueInput::Url =>
        {
            let selection = Selection::parse(Some(first.to_owned()), item)?;
            Ok(Some(WorkspaceTarget::Item {
                kind: selection.kind.context("expected an issue or PR")?,
                input: selection
                    .input
                    .with_context(|| format!("{first} needs a number or URL"))?,
            }))
        }
        _ => {
            ensure!(
                item.is_none(),
                "put pr, issue or resource before a number, URL or name"
            );
            Ok(None)
        }
    }
}

fn render_status(view: &StatusView, json: bool) {
    let status = &view.status;
    let palette = Palette::stdout(json);
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
    if let Some(retry) = &status.turn_retry {
        outln!("Turn retry:    {}", turn_retry_label(retry));
    }
    outln!("Chat:          {}", view.chat.as_deref().unwrap_or("none"));

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
        outln!("Completion:    done ({choice})");
        if let Some(error) = &completion.error {
            outln!("  {}", palette.paint(Style::Warning, error));
        }
    }
    match &inspection.pr_cleanup {
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
    render_recorded("Opened:", &workspace.links.opened);
    render_recorded("Related:", &workspace.links.related);
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
