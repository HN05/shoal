//! What a log line says of an entry.
use crate::model::EventCause;
use crate::{
    forge::{
        items::{change::ItemChange, link::LinkKind},
        updates::UpdateKind,
    },
    model::{SpaceKind, SpaceRef, WorkspaceRef},
};
use shoal_protocol::{
    events::{
        AbsorbEvent, AgentEvent, AlertEvent, BlockerEvent, CommandRanEvent, EventDetails,
        EventSpace, FileEventKind, GateEvent, HandoffEvent, ItemActivityEvent, ItemEvent,
        JobEndedEvent, LinkEvent, MergeEvent, ReviewEvent, ShoalEventDetails, ShoalEventKind,
        SimulatorEvent, SpaceEvent, ThreadEvent,
    },
    log::LogEntry,
    progress::Progress,
};

/// The width labels are padded to: the longest one's.
pub(super) const LABEL_WIDTH: usize = "Approval requested".len();

pub(super) fn describe(entry: &LogEntry) -> (String, String) {
    match entry {
        LogEntry::Event(event) => event_line(&event.details),
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

pub(super) fn space(entry: &LogEntry) -> String {
    match entry {
        LogEntry::Event(event) => event.details.name.to_string(),
        LogEntry::File(event) => event.details.file.space.name.to_string(),
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

fn removed(event: &ShoalEventDetails, error: Option<String>) -> String {
    let names: Vec<_> = event.removed.iter().map(ToString::to_string).collect();
    let names = names.join(", ");
    match error {
        Some(error) => format!("{names}; failed: {error}"),
        None => names,
    }
}

fn event_line(details: &EventDetails) -> (String, String) {
    let fixed = |label: &str, text: String| (label.to_owned(), text);
    let cause_text = || details.cause.map(cause).unwrap_or_default();
    let error = || details.error.clone().unwrap_or_default();
    match &details.event {
        SpaceEvent::Created {
            base_ref,
            base_workspace,
            parent,
        } => fixed(
            "Created",
            created(
                base_ref.as_deref(),
                base_workspace.as_ref(),
                parent.as_ref(),
            ),
        ),
        SpaceEvent::Renamed => fixed("Renamed", details.name.to_string()),
        SpaceEvent::RemovalStarted => fixed("Removal started", cause_text()),
        SpaceEvent::Removed => fixed("Removed", cause_text()),
        SpaceEvent::Restored => fixed("Restored", String::new()),
        SpaceEvent::Converted { handoff } => fixed("Converted", converted(details, handoff)),
        SpaceEvent::Absorbed { absorb } => fixed("Absorbed", absorbed_into(absorb)),
        SpaceEvent::AbsorbedSpace { absorb } => fixed("Absorbed a space", took_over(absorb)),
        SpaceEvent::AgentStarted { agent } => fixed("Agent started", started(agent)),
        SpaceEvent::AgentStopped { agent } => fixed("Agent stopped", stop(agent)),
        SpaceEvent::AgentExited { agent } => fixed("Agent exited", exit(agent)),
        SpaceEvent::ReviewReady { review: mark } => fixed("Marked ready", review(mark)),
        SpaceEvent::ReviewCleared { review: mark } => fixed("Ready withdrawn", review(mark)),
        SpaceEvent::Completed => fixed("Done", String::new()),
        SpaceEvent::Undone => fixed("Done withdrawn", String::new()),
        SpaceEvent::Linked { link: changed } => link(changed, "linked"),
        SpaceEvent::Unlinked { link: changed } => link(changed, "unlinked"),
        SpaceEvent::ItemChanged { item } => item_changed(item),
        SpaceEvent::ItemActivity { item_activity } => item_activity_line(item_activity),
        SpaceEvent::ThreadOpened { thread: opened } => fixed("Thread opened", thread(opened)),
        SpaceEvent::ThreadPosted { thread: posted } => fixed("Thread post", thread(posted)),
        SpaceEvent::ThreadForwarded { thread: forwarded } => {
            fixed("Thread forwarded", thread(forwarded))
        }
        SpaceEvent::ThreadResolved { thread: resolved } => {
            fixed("Thread resolved", resolution(resolved))
        }
        SpaceEvent::ApprovalRequested { approval } => fixed(
            "Approval requested",
            format!("{} {}", approval.number, approval.subject),
        ),
        SpaceEvent::ApprovalDecided { approval } => fixed(
            "Approval decided",
            format!(
                "{} {}: {}",
                approval.number, approval.status, approval.subject
            ),
        ),
        SpaceEvent::GateDecided { gate: verdict } => fixed("Gate decided", gate(verdict)),
        SpaceEvent::AlertSent { alert: sent } => fixed("Alert sent", alert(sent)),
        SpaceEvent::ProgressChanged { progress: changed } => fixed("Progress", progress(changed)),
        SpaceEvent::ChildCreated { child: created } => fixed("Child created", child(created)),
        SpaceEvent::ChildRemoved { child: removed } => fixed("Child removed", child(removed)),
        SpaceEvent::JobEnded { job: ended } => fixed("CI job ended", job(ended)),
        SpaceEvent::CommandRan { command: ran } => fixed("Command ran", command(ran)),
        SpaceEvent::NoMergeRuns { merge: merged } => fixed("No CI run of the merge", merge(merged)),
        SpaceEvent::BlockerAdded { blocker: added } => fixed("Blocker added", blocker(added)),
        SpaceEvent::BlockerRemoved { blocker: removed } => {
            fixed("Blocker removed", blocker(removed))
        }
        SpaceEvent::BlockerCleared { blocker: cleared } => {
            fixed("Blocker cleared", blocker(cleared))
        }
        SpaceEvent::Ready => fixed("Setup finished", String::new()),
        SpaceEvent::SetupFailed => fixed("Setup failed", error()),
        SpaceEvent::Retained => fixed("Removal failed", error()),
        SpaceEvent::BranchChanged => fixed(
            "Branch changed",
            (details.space.branch()).map_or_else(|| "detached HEAD".into(), ToString::to_string),
        ),
        SpaceEvent::BaseChanged { base_workspace } => fixed(
            "Base changed",
            base_workspace
                .as_ref()
                .map_or_else(|| "none".into(), |base| base.name.to_string()),
        ),
        SpaceEvent::ParentChanged { parent } => fixed(
            "Parent changed",
            parent
                .as_ref()
                .map_or_else(|| "none".into(), |parent| parent.name.to_string()),
        ),
        SpaceEvent::AgentState { agent_state } => fixed(
            "Agent state",
            (agent_state.state).map_or_else(|| "ended".into(), |state| state.to_string()),
        ),
        SpaceEvent::DemoStarted { demo } => fixed("Demo started", demo.name.clone()),
        SpaceEvent::DemoStopped { demo } => fixed("Demo stopped", demo.name.clone()),
        SpaceEvent::SimulatorAcquired {
            simulator: acquired,
        } => fixed("Simulator acquired", simulator(acquired)),
        SpaceEvent::SimulatorReleased {
            simulator: released,
        } => fixed("Simulator released", simulator(released)),
    }
}

/// How many steps are done and the status line, as `2/5 steps: Reproduced`.
fn progress(progress: &Progress) -> String {
    let steps = format!("{}/{} steps", progress.done(), progress.steps.len());
    match &progress.status {
        Some(status) => format!("{steps}: {status}"),
        None => steps,
    }
}

fn blocker(event: &BlockerEvent) -> String {
    let mut text = event.blocker.cause();
    if let Some(step) = &event.step {
        text += &format!(" on step {step}");
    }
    text += &format!(": {}", event.blocker.reason);
    if let Some(cleared) = &event.cleared {
        text += &format!("; {cleared}");
    }
    text
}

fn created(
    base_ref: Option<&str>,
    base_workspace: Option<&WorkspaceRef>,
    parent: Option<&SpaceRef>,
) -> String {
    let base = match base_workspace {
        Some(base) => base.branch.as_ref().map(ToString::to_string),
        None => base_ref.map(short_ref),
    };
    let mut parts = Vec::new();
    if let Some(base) = base {
        parts.push(format!("from {base}"));
    }
    if let Some(parent) = parent {
        parts.push(format!("parent {}", parent.name));
    }
    parts.join(", ")
}

fn converted(details: &EventDetails, handoff: &Option<Box<HandoffEvent>>) -> String {
    if let Some(handoff) = handoff {
        return format!(
            "from {} to {}",
            handed_over(&handoff.from),
            handed_over(&handoff.to)
        );
    }
    match &details.space {
        EventSpace::Workspace {
            path,
            branch: Some(branch),
        } => format!("to workspace on {branch} at {}", path.display()),
        EventSpace::Workspace { path, branch: None } => {
            format!("to workspace at {}", path.display())
        }
        EventSpace::Chat => "to chat".to_owned(),
    }
}

fn handed_over(side: &shoal_protocol::transcripts::HandedOverSession) -> String {
    match &side.session {
        Some(session) => format!("{} session {session}", side.agent),
        None => side.agent.to_string(),
    }
}

fn absorbed_into(absorb: &AbsorbEvent) -> String {
    match absorb.method {
        Some(method) => format!("into {} by {method}", absorb.space.name),
        None => format!("into {}", absorb.space.name),
    }
}

fn took_over(absorb: &AbsorbEvent) -> String {
    let mut parts = vec![absorb.space.name.to_string()];
    if let Some(method) = absorb.method {
        parts.push(format!("merged by {method}"));
    }
    let counted = [
        (absorb.carried.len(), "uncommitted files"),
        (absorb.links.len(), "links"),
        (absorb.files.len(), "files"),
        (absorb.threads.len(), "threads"),
        (absorb.children.len(), "children"),
    ];
    for (count, what) in counted {
        if count > 0 {
            parts.push(format!("{count} {what}"));
        }
    }
    parts.join(", ")
}

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
        EventCause::Repository => "repository removed",
        EventCause::ParentCompletion => "parent done",
        EventCause::Absorbed => "base absorbed",
        EventCause::StartFailed => "start failed",
        EventCause::Blocker => "blocker done",
    }
    .to_owned()
}

fn started(agent: &AgentEvent) -> String {
    match &agent.source {
        Some(source) => format!("{}, {source}", agent.name),
        None => agent.name.to_string(),
    }
}

fn exit(agent: &AgentEvent) -> String {
    let exit = match agent.exit_code {
        Some(code) => format!("{}, exit code {code}", agent.name),
        None => format!("{}, no exit code", agent.name),
    };
    match agent.oom_kills {
        0 => exit,
        kills => format!("{exit}, {kills} killed for memory"),
    }
}

fn stop(agent: &AgentEvent) -> String {
    match agent.cause {
        Some(cause) => format!("{}, {cause}", agent.name),
        None => agent.name.to_string(),
    }
}

fn review(review: &ReviewEvent) -> String {
    review.url.as_deref().map(reference).unwrap_or_default()
}

fn link(link: &LinkEvent, change: &str) -> (String, String) {
    let label = format!("{} {change}", kind_name(link.kind));
    (label, format!("{} {}", reference(&link.url), link.relation))
}

fn item_changed(item: &ItemEvent) -> (String, String) {
    let kind = kind_name(item.kind);
    let reference = reference(&item.url);
    let done = match item.action {
        ItemChange::Open => "opened",
        ItemChange::Edit => "edited",
        ItemChange::Close => "closed",
        ItemChange::Reopen => "reopened",
        ItemChange::Merge => "merged",
        ItemChange::Comment => "comment",
        ItemChange::Review => "reviewed",
        ItemChange::Approve => "approved",
        ItemChange::RequestChanges => "changes requested",
        ItemChange::Rerun => "rerun",
        ItemChange::Cancel => "canceled",
        ItemChange::EditComment
        | ItemChange::AutoMerge
        | ItemChange::CancelAutoMerge
        | ItemChange::Update
        | ItemChange::Reply
        | ItemChange::Resolve
        | ItemChange::Unresolve
        | ItemChange::React
        | ItemChange::Unreact => {
            let action = item.action.verb();
            return (format!("{kind} changed"), format!("{reference} {action}"));
        }
    };
    (format!("{kind} {done}"), reference)
}

fn item_activity_line(activity: &ItemActivityEvent) -> (String, String) {
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

fn thread(thread: &ThreadEvent) -> String {
    let mut line = format!("{} {}", thread.number, thread.name.replace('\n', " "));
    match (&thread.sender, &thread.to, &thread.text) {
        (Some(sender), Some(to), _) => line.push_str(&format!(": {sender} to {to}")),
        (Some(sender), None, Some(text)) => {
            line.push_str(&format!(": {sender}: {}", text.replace('\n', " ")))
        }
        _ => {}
    }
    line
}

/// A resolved thread with its outcome, or that it was reopened.
fn resolution(resolved: &ThreadEvent) -> String {
    match resolved.outcome {
        Some(outcome) => format!("{}: {outcome}", thread(resolved)),
        None => format!("{}: reopened", thread(resolved)),
    }
}

fn gate(gate: &GateEvent) -> String {
    let rule = (gate.rule.as_ref()).map_or_else(String::new, |rule| format!(" ({rule})"));
    format!(
        "{} {}: {}: {}{rule}",
        gate.number, gate.verdict, gate.subject, gate.reason
    )
}

fn alert(alert: &AlertEvent) -> String {
    let message = alert.message.replace('\n', " ");
    if alert.alerts.is_empty() {
        return message;
    }
    let numbers: Vec<String> = alert.alerts.iter().map(i64::to_string).collect();
    format!("{} {message}", numbers.join(", "))
}

fn child(child: &SpaceRef) -> String {
    match child.kind() {
        SpaceKind::Chat => format!("{} (chat)", child.name),
        _ => child.name.to_string(),
    }
}

fn job(job: &JobEndedEvent) -> String {
    let ended = format!("{}, {}; {}", job.name, reference(&job.run), job.reason);
    if job.stopped.is_empty() {
        return ended;
    }
    let mut stopped: Vec<_> = (job.stopped.iter())
        .map(|process| format!("{} {}", process.pid, process.command))
        .collect();
    if job.omitted > 0 {
        stopped.push(format!("{} more", job.omitted));
    }
    format!("{ended}; stopped {}", stopped.join(", "))
}

/// A `shoal run` command's run, as `test: peak 7.2 GiB in 3m 12s,
/// stopped`.
fn command(ran: &CommandRanEvent) -> String {
    let took = std::time::Duration::from_millis(ran.duration_ms);
    let mut text = format!(
        "{}: peak {} in {}",
        ran.command,
        crate::cli::ui::size_label(ran.peak),
        crate::cli::ui::duration_label(took)
    );
    if ran.oom_killed {
        text += ", the OOM killer killed a process of it";
    } else if !ran.whole {
        text += ", stopped";
    }
    text
}

/// The merged PR and its merge commit, as `#12 at 0a1b2c3`, or the
/// commit a pushed merge left, as `pushed 0a1b2c3`.
fn merge(merge: &MergeEvent) -> String {
    let commit = merge.commit.short();
    match &merge.pr {
        Some(pr) => format!("{} at {commit}", reference(pr)),
        None => format!("pushed {commit}"),
    }
}

fn simulator(simulator: &SimulatorEvent) -> String {
    format!("{} {}", simulator.device, simulator.runtime)
}

#[cfg(test)]
mod tests;
