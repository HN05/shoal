//! `shoal inbox`: what needs the user now, each with its workspace: open
//! questions, pending approvals, agents that need input, failed workspaces,
//! work marked ready at its current commit, running demos, and Git worktrees
//! under Shoal's directories that no workspace owns.
use std::collections::HashSet;

use anyhow::{Result, ensure};
use serde::Serialize;

use super::listing::ListFilter;
use crate::{
    cli::{
        client::{self, request},
        context::Context,
        list_status::ListStatus,
        output::{self, Palette, Style},
        ui::{self, Cell, WidthLimit},
    },
    daemon::{
        approvals::{ApprovalRequest, DecisionStatus},
        questions::{Listing, Question},
    },
    demos::Running,
    model::{ReviewMark, UntrackedWorktree, Workspace},
    protocol::Method,
    state::WorkspaceState,
};

/// Something that needs the user.
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(in crate::cli) enum Item {
    Question {
        question: Question,
    },
    Approval {
        request: ApprovalRequest,
    },
    NeedsInput {
        workspace: Workspace,
    },
    Failed {
        workspace: Workspace,
    },
    Ready {
        workspace: Workspace,
        mark: ReviewMark,
    },
    Demo {
        demo: Running,
    },
    /// A worktree with no workspace, so it belongs to no listed one.
    Untracked {
        worktree: UntrackedWorktree,
    },
}

/// The inbox's items in the order it lists them, and the workspaces they
/// belong to.
pub(in crate::cli) struct Inbox {
    pub items: Vec<Item>,
    workspaces: Vec<Workspace>,
}

pub(super) async fn list(ctx: &Context, filter: &ListFilter) -> Result<i32> {
    let inbox = Inbox::request(ctx, filter).await?;
    // Only text rows name repositories, which reads each checkout's remote.
    let repositories = if ctx.json {
        Vec::new()
    } else {
        ui::repository_choices(client::repositories(&ctx.paths).await?).await?
    };
    let rows = inbox.rows(
        &repositories,
        Palette::stdout(ctx.json),
        output::stdout_width(ctx.json),
    );
    ctx.show(&inbox.items, |_| {
        for row in &rows {
            outln!("{row}");
        }
        if rows.is_empty() {
            outln!("Nothing needs you");
        }
    })?;
    Ok(0)
}

impl Inbox {
    /// Questions, then approvals, agents that need input, failed workspaces,
    /// ready marks and running demos, for the workspaces `filter` keeps.
    pub(in crate::cli) async fn request(ctx: &Context, filter: &ListFilter) -> Result<Self> {
        // Without its token, the daemon would answer a workspace process as the user.
        ensure!(
            crate::env::is_scoped() || !crate::env::inherits_scope(&ctx.paths.state),
            "a workspace process that cleared its scope token cannot list the inbox"
        );
        let mut workspaces = client::workspaces(&ctx.paths).await?;
        let stopped = ui::stopped_workspaces(&ctx.paths, &workspaces);
        let existing: HashSet<String> = workspaces.iter().map(|w| w.id.clone()).collect();
        workspaces.retain(|w| filter.keeps_workspace(w, stopped.contains(&w.id)));
        let questions = request::<Listing>(&ctx.paths, Method::ListQuestions { workspace: None })
            .await?
            .questions;
        let requests =
            request::<Vec<ApprovalRequest>>(&ctx.paths, Method::ListApprovals { workspace: None })
                .await?;
        let detached = |request: &ApprovalRequest| {
            !existing.contains(&request.workspace_id)
                && request
                    .repository_id
                    .as_deref()
                    .is_some_and(|repository| filter.keeps_detached(repository))
        };
        let demos =
            request::<Vec<Running>>(&ctx.paths, Method::ListDemos { workspace: None }).await?;
        let mut items = decision_items(questions, requests, &workspaces, detached);
        items.extend(workspace_items(&workspaces, &stopped));
        items.extend(
            demos
                .into_iter()
                .filter(|demo| workspaces.iter().any(|w| w.id == demo.workspace_id))
                .map(|demo| Item::Demo { demo }),
        );
        // Workspace processes see their own tree, which no untracked worktree is in.
        if !crate::env::is_scoped() {
            let untracked =
                request::<Vec<UntrackedWorktree>>(&ctx.paths, Method::UntrackedWorktrees).await?;
            items.extend(
                untracked
                    .into_iter()
                    .filter(|worktree| filter.keeps_detached(&worktree.repository_id))
                    .map(|worktree| Item::Untracked { worktree }),
            );
        }
        Ok(Self { items, workspaces })
    }

    /// One aligned row per item: its kind, workspace, the workspace's
    /// repository when the rows span several, what it is about, cut to fit
    /// `width`, and a note.
    pub(in crate::cli) fn rows(
        &self,
        repositories: &[(String, String)],
        palette: Palette,
        width: Option<usize>,
    ) -> Vec<String> {
        let repository = |item| repository_of(&self.workspaces, item);
        let cells: Vec<[Cell; 5]> = self
            .items
            .iter()
            .zip(ui::repository_cells(
                self.items.iter().map(repository),
                repositories,
            ))
            .map(|(item, repository)| {
                [
                    item.kind(),
                    (item.workspace_name().into(), None),
                    (repository, None),
                    (item.text(), None),
                    (item.note(), Some(Style::Muted)),
                ]
            })
            .collect();
        let limit = width.map(|width| WidthLimit { column: 3, width });
        ui::aligned_to(cells, palette, limit)
    }
}

/// Open questions, then the pending approval requests the user decides, of
/// `workspaces`; a coordinating agent decides the others.
/// The repository of the item's workspace, or the one that owns a proposed
/// issue whose workspace is gone.
fn repository_of<'a>(workspaces: &'a [Workspace], item: &'a Item) -> &'a str {
    workspaces
        .iter()
        .find(|w| w.id == item.workspace_id())
        .map(|w| w.repository_id.as_str())
        .or_else(|| item.repository_id())
        .unwrap_or("")
}

/// Questions and approvals of the listed workspaces, and the requests
/// `detached` keeps: issue proposals that outlived their workspace.
fn decision_items(
    questions: Vec<Question>,
    requests: Vec<ApprovalRequest>,
    workspaces: &[Workspace],
    detached: impl Fn(&ApprovalRequest) -> bool,
) -> Vec<Item> {
    let kept = |id: &str| workspaces.iter().any(|w| w.id == id);
    let questions = questions
        .into_iter()
        .filter(|question| kept(&question.workspace_id))
        .map(|question| Item::Question { question });
    let requests = requests
        .into_iter()
        .filter(|request| {
            request.status == DecisionStatus::Pending
                && request.decider.is_none()
                && (kept(&request.workspace_id) || detached(request))
        })
        .map(|request| Item::Approval { request });
    questions.chain(requests).collect()
}

/// Workspaces whose agent needs input, then failed ones, then each mark made at
/// the current commit.
fn workspace_items(workspaces: &[Workspace], stopped: &HashSet<String>) -> Vec<Item> {
    let status = |w: &Workspace| ListStatus::of_workspace(w, stopped.contains(&w.id));
    let waiting = workspaces
        .iter()
        .filter(|w| status(w) == ListStatus::NeedsInput)
        .map(|w| Item::NeedsInput {
            workspace: w.clone(),
        });
    let failed = workspaces
        .iter()
        .filter(|w| w.state == WorkspaceState::Failed)
        .map(|w| Item::Failed {
            workspace: w.clone(),
        });
    let review = workspaces.iter().flat_map(|w| {
        w.review
            .iter()
            .filter(|mark| mark.stale != Some(true))
            .map(|mark| Item::Ready {
                workspace: w.clone(),
                mark: mark.clone(),
            })
    });
    waiting.chain(failed).chain(review).collect()
}

impl Item {
    pub(in crate::cli) fn workspace_id(&self) -> &str {
        match self {
            Self::Question { question } => &question.workspace_id,
            Self::Approval { request } => &request.workspace_id,
            Self::NeedsInput { workspace }
            | Self::Failed { workspace }
            | Self::Ready { workspace, .. } => &workspace.id,
            Self::Demo { demo } => &demo.workspace_id,
            Self::Untracked { .. } => "",
        }
    }

    /// The repository that owns a proposed issue, which outlives the
    /// workspace that proposed it, or an untracked worktree.
    fn repository_id(&self) -> Option<&str> {
        match self {
            Self::Approval { request } => request.repository_id.as_deref(),
            Self::Untracked { worktree } => Some(&worktree.repository_id),
            _ => None,
        }
    }

    fn workspace_name(&self) -> &str {
        match self {
            Self::Question { question } => &question.workspace,
            Self::Approval { request } => &request.workspace,
            Self::NeedsInput { workspace }
            | Self::Failed { workspace }
            | Self::Ready { workspace, .. } => &workspace.name,
            Self::Demo { demo } => &demo.workspace,
            Self::Untracked { .. } => "",
        }
    }

    fn kind(&self) -> Cell {
        let (label, style) = match self {
            Self::Question { .. } => ("question", Style::Warning),
            Self::Approval { .. } => ("approval", Style::Warning),
            Self::NeedsInput { .. } => ("needs input", Style::Warning),
            Self::Failed { .. } => ("failed", Style::Error),
            Self::Ready { .. } => ("ready", Style::Success),
            Self::Demo { .. } => ("demo", Style::Success),
            Self::Untracked { .. } => ("untracked", Style::Warning),
        };
        (label.into(), Some(style))
    }

    /// What the item asks or reports: the question, the access requested
    /// and why, the failure, or the marked item with the issue's title.
    fn text(&self) -> String {
        let title = |w: &Workspace| w.links.issue_title.clone().unwrap_or_default();
        match self {
            Self::Question { question } => question.ask.question.clone(),
            Self::Approval { request } => super::approvals::about(request),
            Self::NeedsInput { workspace } => title(workspace),
            Self::Failed { workspace } => workspace
                .error
                .as_deref()
                .and_then(|error| error.lines().next())
                .map_or_else(|| title(workspace), str::to_owned),
            // A mark on the workspace itself needs no label beside its name.
            Self::Ready { workspace, mark } if mark.url.is_none() => title(workspace),
            Self::Ready { workspace, mark } => {
                let label = ui::mark_label(mark);
                match &workspace.links.issue_title {
                    Some(issue) => format!("{label} {issue}"),
                    None => label,
                }
            }
            Self::Demo { demo } => demo.launch.name.clone(),
            Self::Untracked { worktree } => worktree.path.display().to_string(),
        }
    }

    fn note(&self) -> String {
        match self {
            Self::Question { question } => format!("recommends {}", question.ask.recommendation),
            Self::Untracked { .. } => "adopt it with shoal work adopt".to_owned(),
            _ => String::new(),
        }
    }
}
