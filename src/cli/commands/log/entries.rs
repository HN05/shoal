//! What a log line says of an entry: a short label and its details.
use crate::{
    client::{
        events::{
            ChatEventKind, EventCause, EventDetails, EventKind, FileEventKind, ShoalEventDetails,
            ShoalEventKind,
        },
        log::LogEntry,
    },
    forge::{items::link::LinkKind, updates::UpdateKind},
    model::SpaceKind,
};

/// The width labels are padded to: the longest one's.
pub(super) const LABEL_WIDTH: usize = "Approval requested".len();

/// The label and details of `entry`.
pub(super) fn describe(entry: &LogEntry) -> (String, String) {
    match entry {
        LogEntry::Event(event) => event_line(&event.details),
        LogEntry::Chat(event) => {
            let chat = &event.chat;
            let (label, details) = match event.kind {
                ChatEventKind::Started => ("Created", chat.agent.to_string()),
                ChatEventKind::Renamed => ("Renamed", chat.name.to_string()),
                ChatEventKind::Saved => ("Agent stopped", chat.agent.to_string()),
                ChatEventKind::Restored => ("Agent started", chat.agent.to_string()),
                ChatEventKind::Ended => ("Removed", String::new()),
            };
            (label.to_owned(), details)
        }
        LogEntry::File(event) => {
            let label = match event.details.kind {
                FileEventKind::Added => "File added",
                FileEventKind::Removed => "File removed",
            };
            (label.to_owned(), event.details.file.name.clone())
        }
        LogEntry::Shoal(event) => shoal_line(&event.details),
    }
}

/// The name of the space `entry` belongs to, or `Shoal` for Shoal's own.
pub(super) fn space(entry: &LogEntry) -> String {
    match entry {
        LogEntry::Event(event) => event.details.name.to_string(),
        LogEntry::Chat(event) => event.chat.name.to_string(),
        LogEntry::File(event) => {
            let file = &event.details.file;
            (file.workspace.as_ref().or(file.chat.as_ref()))
                .map_or_else(String::new, ToString::to_string)
        }
        LogEntry::Shoal(_) => "Shoal".to_owned(),
    }
}

fn shoal_line(event: &ShoalEventDetails) -> (String, String) {
    let error = event.error.clone();
    let (label, details) = match event.kind {
        ShoalEventKind::DaemonStarted => ("Daemon started", event.version.clone()),
        ShoalEventKind::DaemonStopped => ("Daemon stopped", error),
        ShoalEventKind::DaemonRestarted => ("Daemon restarted", None),
        ShoalEventKind::ConfigReloaded if error.is_some() => ("Config rejected", error),
        ShoalEventKind::ConfigReloaded => ("Config reloaded", None),
        ShoalEventKind::CleanupRan if event.removed.is_empty() => ("Cleanup failed", error),
        ShoalEventKind::CleanupRan => ("Cleanup removed", Some(removed(event, error))),
    };
    (label.to_owned(), details.unwrap_or_default())
}

/// The spaces a cleanup run removed, and its `error` when another step
/// failed.
fn removed(event: &ShoalEventDetails, error: Option<String>) -> String {
    let names: Vec<_> = event.removed.iter().map(ToString::to_string).collect();
    let names = names.join(", ");
    match error {
        Some(error) => format!("{names}; failed: {error}"),
        None => names,
    }
}

fn event_line(event: &EventDetails) -> (String, String) {
    let fixed = |label: &str, details: String| (label.to_owned(), details);
    match event.kind {
        EventKind::Created => fixed("Created", created(event)),
        EventKind::Renamed => fixed("Renamed", renamed(event)),
        EventKind::Removed => fixed("Removed", event.cause.map(cause).unwrap_or_default()),
        EventKind::Restored => fixed("Restored", String::new()),
        EventKind::Converted => fixed("Converted", converted(event)),
        EventKind::AgentStarted => fixed("Agent started", agent(event)),
        EventKind::AgentStopped => fixed("Agent stopped", agent(event)),
        EventKind::AgentExited => fixed("Agent exited", exit(event)),
        EventKind::ReviewReady => fixed("Marked ready", review(event)),
        EventKind::ReviewCleared => fixed("Ready withdrawn", review(event)),
        EventKind::Completed => fixed("Done", String::new()),
        EventKind::Undone | EventKind::Continued => fixed("Done withdrawn", String::new()),
        EventKind::Linked | EventKind::Unlinked => link(event),
        EventKind::ItemChanged => item_changed(event),
        EventKind::ItemActivity => item_activity(event),
        EventKind::QuestionAsked => fixed("Question asked", question(event, false)),
        EventKind::QuestionAnswered => fixed("Question answered", question(event, true)),
        EventKind::ApprovalRequested => fixed("Approval requested", approval(event, false)),
        EventKind::ApprovalDecided => fixed("Approval decided", approval(event, true)),
        EventKind::AlertSent => fixed(
            "Alert sent",
            (event.alert.as_ref()).map_or_else(String::new, |alert| alert.message.clone()),
        ),
        EventKind::ProgressChanged => fixed("Progress", progress(event)),
        EventKind::ChildCreated => fixed("Child created", child(event)),
        EventKind::ChildRemoved => fixed("Child removed", child(event)),
        EventKind::Ready => fixed("Setup finished", String::new()),
        EventKind::SetupFailed => fixed("Setup failed", error(event)),
        EventKind::Retained => fixed("Cleanup failed", error(event)),
        EventKind::BranchChanged => fixed(
            "Branch changed",
            event
                .branch
                .clone()
                .unwrap_or_else(|| "detached HEAD".into()),
        ),
        EventKind::BaseChanged => fixed("Base changed", base(event)),
        EventKind::ParentChanged | EventKind::ChatChanged => fixed("Parent changed", parent(event)),
        EventKind::AgentState => fixed(
            "Agent state",
            (event.agent_state.as_ref().and_then(|state| state.state))
                .map_or_else(|| "ended".into(), |state| state.to_string()),
        ),
        EventKind::DemoStarted => fixed("Demo started", demo(event)),
        EventKind::DemoStopped => fixed("Demo stopped", demo(event)),
        EventKind::SimulatorAcquired => fixed("Simulator acquired", simulator(event)),
        EventKind::SimulatorReleased => fixed("Simulator released", simulator(event)),
    }
}

/// How many steps are done and the status line, as `2/5 steps: Reproduced`.
fn progress(event: &EventDetails) -> String {
    let Some(progress) = &event.progress else {
        return String::new();
    };
    let steps = format!("{}/{} steps", progress.done(), progress.steps.len());
    match &progress.status {
        Some(status) => format!("{steps}: {status}"),
        None => steps,
    }
}

/// Where a workspace started and the space it is below.
fn created(event: &EventDetails) -> String {
    let base = match event.base_workspace.clone().flatten() {
        Some(base) => base.branch.map(|branch| branch.to_string()),
        None => event.base_ref.as_deref().map(short_ref),
    };
    let mut parts = Vec::new();
    if let Some(base) = base {
        parts.push(format!("from {base}"));
    }
    if let Some(parent) = event.parent.clone().flatten() {
        parts.push(format!("parent {}", parent.name));
    }
    parts.join(", ")
}

/// What the space became: a chat, or a workspace on its branch at its path.
fn converted(event: &EventDetails) -> String {
    match (&event.branch, &event.path) {
        (Some(branch), Some(path)) => format!("to workspace on {branch} at {}", path.display()),
        _ => "to chat".to_owned(),
    }
}

/// A workspace's new title, or its new name and a branch named otherwise.
fn renamed(event: &EventDetails) -> String {
    match &event.title {
        Some(title) => title.clone().unwrap_or_default(),
        None => match event.branch.as_deref() {
            Some(branch) if branch != event.name => format!("{}, branch {branch}", event.name),
            _ => event.name.to_string(),
        },
    }
}

/// A branch or tag without its `refs/` prefix.
fn short_ref(reference: &str) -> String {
    ["refs/heads/", "refs/tags/", "refs/remotes/"]
        .iter()
        .find_map(|prefix| reference.strip_prefix(prefix))
        .unwrap_or(reference)
        .to_owned()
}

fn cause(cause: EventCause) -> String {
    match cause {
        EventCause::Manual => "by request",
        EventCause::Idle => "idle",
        EventCause::Issue => "issue closed",
        EventCause::Pr => "PR merged",
        EventCause::Completion => "done",
        EventCause::MissingDirectory => "directory missing",
        EventCause::DiskSpace => "low disk space",
        EventCause::Merged => "branch merged",
        EventCause::Removed => "base removed",
        EventCause::Parent => "parent removed",
        EventCause::ParentCompletion => "parent done",
    }
    .to_owned()
}

fn agent(event: &EventDetails) -> String {
    (event.agent.as_ref()).map_or_else(String::new, |agent| agent.name.to_string())
}

fn exit(event: &EventDetails) -> String {
    match &event.agent {
        Some(agent) => match agent.exit_code {
            Some(code) => format!("{}, exit code {code}", agent.name),
            None => format!("{}, no exit code", agent.name),
        },
        None => String::new(),
    }
}

/// The marked item, or the commit a mark of the space itself is at.
fn review(event: &EventDetails) -> String {
    let Some(review) = &event.review else {
        return String::new();
    };
    let head = short_commit(&review.head);
    match &review.url {
        Some(url) => format!("{} at {head}", reference(url)),
        None => format!("at {head}"),
    }
}

fn short_commit(commit: &str) -> String {
    commit.chars().take(7).collect()
}

fn link(event: &EventDetails) -> (String, String) {
    let Some(link) = &event.link else {
        return ("Link changed".into(), String::new());
    };
    let change = match event.kind {
        EventKind::Unlinked => "unlinked",
        _ => "linked",
    };
    let label = format!("{} {change}", kind_name(link.kind));
    (label, format!("{} {}", reference(&link.url), link.relation))
}

/// Shoal's change to an item, by the action's past form where it has one.
fn item_changed(event: &EventDetails) -> (String, String) {
    let Some(item) = &event.item else {
        return ("Item changed".into(), String::new());
    };
    let kind = kind_name(item.kind);
    let reference = reference(&item.url);
    let done = match item.action.as_str() {
        "open" => "opened",
        "edit" => "edited",
        "close" => "closed",
        "reopen" => "reopened",
        "merge" => "merged",
        "comment" => "comment",
        "review" => "reviewed",
        "approve" => "approved",
        "rerun" => "rerun",
        "cancel" => "canceled",
        action => {
            let action = action.replace('_', " ");
            return (format!("{kind} changed"), format!("{reference} {action}"));
        }
    };
    (format!("{kind} {done}"), reference)
}

/// An item's closing, merging or reopening on its forge, or with every
/// event other activity.
fn item_activity(event: &EventDetails) -> (String, String) {
    let Some(activity) = &event.item_activity else {
        return ("Item activity".into(), String::new());
    };
    let kind = kind_name(activity.kind);
    let reference = reference(&activity.url);
    let done = match activity.update.kind {
        UpdateKind::Closed => "closed",
        UpdateKind::Merged => "merged",
        UpdateKind::Reopened => "reopened",
        _ => {
            let message = &activity.update.message;
            return (format!("{kind} activity"), format!("{reference} {message}"));
        }
    };
    (format!("{kind} {done}"), reference)
}

fn kind_name(kind: LinkKind) -> &'static str {
    match kind {
        LinkKind::Issue => "Issue",
        LinkKind::Pr => "PR",
        LinkKind::Ci => "CI run",
    }
}

/// An issue's or PR's number as `#12`, a CI run's as `run 2765`, else the URL.
fn reference(url: &str) -> String {
    let mut segments = url.trim_end_matches('/').rsplit('/');
    match (segments.next(), segments.next()) {
        (Some(number), Some("runs")) if number.parse::<u64>().is_ok() => format!("run {number}"),
        (Some(number), Some(_)) if number.parse::<u64>().is_ok() => format!("#{number}"),
        _ => url.to_owned(),
    }
}

fn question(event: &EventDetails, answered: bool) -> String {
    let Some(question) = &event.question else {
        return String::new();
    };
    let text = match (answered, &question.answer) {
        (true, Some(answer)) => answer,
        _ => &question.question,
    };
    format!("{} {text}", question.number)
}

fn approval(event: &EventDetails, decided: bool) -> String {
    let Some(approval) = &event.approval else {
        return String::new();
    };
    match decided {
        true => format!(
            "{} {}: {}",
            approval.number, approval.status, approval.subject
        ),
        false => format!("{} {}", approval.number, approval.subject),
    }
}

fn child(event: &EventDetails) -> String {
    match &event.child {
        Some(child) if child.kind == SpaceKind::Chat => format!("{} (chat)", child.name),
        Some(child) => child.name.to_string(),
        None => String::new(),
    }
}

fn error(event: &EventDetails) -> String {
    event.error.clone().unwrap_or_default()
}

fn base(event: &EventDetails) -> String {
    match event.base_workspace.clone().flatten() {
        Some(base) => base.name.to_string(),
        None => "none".into(),
    }
}

fn parent(event: &EventDetails) -> String {
    match event.parent.clone().flatten() {
        Some(parent) => parent.name.to_string(),
        None => "none".into(),
    }
}

fn demo(event: &EventDetails) -> String {
    (event.demo.as_ref()).map_or_else(String::new, |demo| demo.name.clone())
}

fn simulator(event: &EventDetails) -> String {
    (event.simulator.as_ref()).map_or_else(String::new, |simulator| {
        format!("{} {}", simulator.device, simulator.runtime)
    })
}

#[cfg(test)]
mod tests;
