//! The rows each view lists, and the picker ids that tell what a chosen row
//! stands for.
use std::{
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};

use super::View;
use crate::cli::commands::{inbox::Inbox, listing::ListFilter};
use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceSelector;
use crate::client::{item_numbers::ItemNumber, output::inbox::Item};
use crate::{
    cli::{
        context::Context,
        output::{Palette, Style},
        ui::{self, Entries, WidthLimit},
    },
    client::notifications::{self, Notification},
};

const ADD_ENTRY: &str = "add-workspace";

/// How many recent notifications the notifications view lists.
const NOTIFICATIONS_SHOWN: u32 = 50;

/// What a row stands for, by the picker id it is chosen with: an inbox
/// item with its space's ID, a workspace, or the entry that adds one.
pub(super) enum Row {
    /// A question by its number, of a workspace or chat.
    Question {
        number: ItemNumber,
        space: SpaceId,
    },
    /// A request by its number and when it was made, since a later request
    /// may reuse the number.
    Approval {
        number: ItemNumber,
        created_at: crate::time::UnixTime,
        workspace: SpaceId,
    },
    NeedsInput(SpaceId),
    Failed(SpaceId),
    /// A mark of a workspace or chat, on its item or on itself.
    Ready {
        space: SpaceId,
        url: Option<String>,
    },
    /// A running demo, by its name.
    Demo {
        workspace: SpaceId,
        name: String,
    },
    /// A worktree no workspace owns, by its path.
    Untracked(PathBuf),
    /// Automatic cleanup failing other than in a workspace.
    Cleanup,
    /// A repository whose default branch stopped syncing, by its ID.
    SyncFailed(String),
    Workspace(SpaceId),
    /// A notification, by the name its workspace had, if any.
    Notification(String),
    Add,
}

impl Row {
    fn of(item: &Item) -> Self {
        match item {
            Item::Question { question } => Self::Question {
                number: question.number,
                space: question.space_id.clone(),
            },
            Item::Approval { request } => Self::Approval {
                number: request.number,
                created_at: request.created_at,
                workspace: request.space_id.clone(),
            },
            Item::NeedsInput { workspace } => Self::NeedsInput(workspace.id.clone()),
            Item::ChatNeedsInput { chat } => Self::NeedsInput(chat.id.clone()),
            Item::Failed { workspace } => Self::Failed(workspace.id.clone()),
            Item::Ready { workspace, mark } => Self::Ready {
                space: workspace.id.clone(),
                url: mark.url.clone(),
            },
            Item::ChatReady { chat, mark } => Self::Ready {
                space: chat.id.clone(),
                url: mark.url.clone(),
            },
            // Enter attaches to the chat, whose agent says what it finished.
            Item::ChatDone { chat } => Self::Ready {
                space: chat.id.clone(),
                url: None,
            },
            Item::Demo { demo } => Self::Demo {
                workspace: demo.workspace_id.clone(),
                name: demo.launch.name.clone(),
            },
            Item::Untracked { worktree } => Self::Untracked(worktree.path.clone()),
            Item::Cleanup { .. } => Self::Cleanup,
            Item::SyncFailed { sync } => Self::SyncFailed(sync.repository_id.clone()),
        }
    }

    /// Its picker id: the kind and its fields, colon-separated, with a URL
    /// last since it holds colons itself; a workspace row is its ID.
    fn id(&self) -> String {
        match self {
            Self::Question { number, space } => format!("question:{space}:{number}"),
            Self::Approval {
                number,
                created_at,
                workspace,
            } => format!("approval:{workspace}:{number}:{created_at}"),
            Self::NeedsInput(workspace) => format!("needs-input:{workspace}"),
            Self::Failed(workspace) => format!("failed:{workspace}"),
            Self::Ready { space, url } => {
                format!("ready:{space}:{}", url.as_deref().unwrap_or_default())
            }
            Self::Demo { workspace, name } => format!("demo:{workspace}:{name}"),
            Self::Untracked(path) => format!("untracked:{}", encode_path(path)),
            Self::Cleanup => "cleanup:".into(),
            Self::SyncFailed(repository) => format!("sync-failed:{repository}"),
            Self::Workspace(workspace) => workspace.to_string(),
            Self::Notification(name) => format!("notification:{name}"),
            Self::Add => ADD_ENTRY.into(),
        }
    }

    pub(super) fn parse(id: String) -> Result<Self> {
        if id == ADD_ENTRY {
            return Ok(Self::Add);
        }
        let Some((kind, rest)) = id.split_once(':') else {
            return Ok(Self::Workspace(SpaceId::from(id)));
        };
        if kind == "notification" {
            return Ok(Self::Notification(rest.to_owned()));
        }
        if kind == "untracked" {
            return Ok(Self::Untracked(decode_path(rest)?));
        }
        if kind == "cleanup" {
            return Ok(Self::Cleanup);
        }
        if kind == "sync-failed" {
            return Ok(Self::SyncFailed(rest.to_owned()));
        }
        let (space, detail) = rest.split_once(':').unwrap_or((rest, ""));
        let space = SpaceId::from(space.to_owned());
        let number = || detail.parse().context("picker returned an invalid row");
        Ok(match kind {
            "question" => Self::Question {
                number: number()?,
                space,
            },
            "approval" => {
                let (number, created_at) = detail.split_once(':').unwrap_or((detail, ""));
                let invalid = || anyhow::anyhow!("picker returned an invalid row");
                Self::Approval {
                    number: number.parse().map_err(|_| invalid())?,
                    created_at: created_at.parse().map_err(|_| invalid())?,
                    workspace: space,
                }
            }
            "needs-input" => Self::NeedsInput(space),
            "failed" => Self::Failed(space),
            "ready" => Self::Ready {
                space,
                url: (!detail.is_empty()).then(|| detail.to_owned()),
            },
            "demo" => Self::Demo {
                workspace: space,
                name: detail.to_owned(),
            },
            _ => anyhow::bail!("picker returned an unknown row"),
        })
    }

    /// The workspace or chat the row belongs to, which keys act on.
    pub(super) fn space(&self) -> Option<SpaceSelector> {
        match self {
            Self::Question { space, .. }
            | Self::Approval {
                workspace: space, ..
            }
            | Self::NeedsInput(space)
            | Self::Failed(space)
            | Self::Ready { space, .. }
            | Self::Demo {
                workspace: space, ..
            }
            | Self::Workspace(space) => Some(SpaceSelector::id(space)),
            Self::Notification(name) => (!name.is_empty()).then(|| SpaceSelector::parse(name)),
            Self::Untracked(_) | Self::Cleanup | Self::SyncFailed(_) | Self::Add => None,
        }
    }
}

/// A path as hex digits of its bytes, since a picker id must survive the
/// tabs, newlines and surrounding spaces a path may hold.
fn encode_path(path: &Path) -> String {
    path.as_os_str()
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn decode_path(hex: &str) -> Result<PathBuf> {
    let invalid = || anyhow::anyhow!("picker returned an invalid row");
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|at| {
            hex.get(at..at + 2)
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                .ok_or_else(invalid)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

/// The rows `view` lists for a caller with this scope.
pub(super) async fn rows(ctx: &Context, view: View, scoped: bool) -> Result<Entries> {
    match view {
        View::Inbox => inbox_rows(ctx).await,
        View::Work => work_rows(ctx, scoped).await,
        View::Notifications => notification_rows(ctx).await,
    }
}

/// Recent notifications, newest first; showing the unread ones marks them
/// read, as `shoal alerts` does.
async fn notification_rows(ctx: &Context) -> Result<Entries> {
    let notifications = notifications::list(&ctx.paths, false, NOTIFICATIONS_SHOWN).await?;
    let labels = notification_labels(&notifications);
    mark_read(ctx, &notifications).await?;
    Ok(notifications
        .iter()
        .map(|notification| {
            Row::Notification(notification.workspace.clone().unwrap_or_default()).id()
        })
        .zip(labels)
        .collect())
}

/// One aligned row per notification: when, its workspace, and the message,
/// dimmed once read and cut to fit the picker.
fn notification_labels(notifications: &[Notification]) -> Vec<String> {
    let now = crate::time::UnixTime::now();
    let cells = notifications
        .iter()
        .map(|notification| {
            [
                (notification.created_at.local_ago(now), Some(Style::Muted)),
                (
                    notification.workspace.clone().unwrap_or_else(|| "-".into()),
                    Some(Style::Heading),
                ),
                (
                    notification.message.clone(),
                    notification.read.then_some(Style::Muted),
                ),
            ]
        })
        .collect();
    let limit = ui::picker_width().map(|width| WidthLimit { column: 2, width });
    ui::aligned_to(cells, Palette::picker(), limit)
}

/// Mark the unread ones of `notifications` read.
async fn mark_read(ctx: &Context, notifications: &[Notification]) -> Result<()> {
    let unread: Vec<_> = notifications
        .iter()
        .filter(|notification| !notification.read)
        .map(|notification| notification.id)
        .collect();
    if unread.is_empty() {
        return Ok(());
    }
    notifications::mark_read(&ctx.paths, unread).await
}

/// One row per item that needs the user.
async fn inbox_rows(ctx: &Context) -> Result<Entries> {
    let filter = ListFilter::resolve(ctx, None, Vec::new()).await?;
    let inbox = Inbox::request(ctx, &filter).await?;
    let repos = ui::repository_choices(crate::client::repositories::list(&ctx.paths).await?).await;
    let labels = inbox.rows(&repos, Palette::picker(), ui::picker_width());
    Ok(inbox
        .items
        .iter()
        .map(|item| Row::of(item).id())
        .zip(labels)
        .collect())
}

/// Every workspace, then an entry that adds one unless workspace processes ask.
async fn work_rows(ctx: &Context, scoped: bool) -> Result<Entries> {
    let repos = ui::repository_choices(crate::client::repositories::list(&ctx.paths).await?).await;
    let workspaces = crate::client::workspaces::list(&ctx.paths).await?;
    let palette = Palette::picker();
    let stopped = ui::stopped_workspaces(&ctx.paths, &workspaces);
    let rows = ui::workspace_rows(&workspaces, &repos, &stopped, palette, ui::picker_width());
    let mut entries: Entries = workspaces
        .into_iter()
        .map(|w| w.id.into())
        .zip(rows)
        .collect();
    if !scoped {
        entries.push((
            ADD_ENTRY.into(),
            format!("{} Add workspace", palette.paint(Style::Success, "+")),
        ));
    }
    Ok(entries)
}

#[cfg(test)]
mod tests;
