//! The agent queue's rows: each entry's place, what it waits for and what
//! it starts.
use super::*;

fn queue_status_cell(entry: &QueueEntry) -> Cell {
    let status = ListStatus::of_entry(entry);
    let label = status.label().to_owned();
    let mut text = match entry.position {
        Some(position) => format!("{label}, position {position}"),
        None => label,
    };
    if let Some(rank) = (entry.rank.as_ref()).filter(|_| entry.state == QueueState::Queued) {
        text += &format!(", {}", rank_label(rank));
    }
    (text + &waiting_reasons(entry), Some(status.style()))
}

/// What a queue entry waits for besides its place, each after a comma:
/// `, blocked by api`, `, after an update` or `, held by low disk space`.
pub(super) fn waiting_reasons(entry: &QueueEntry) -> String {
    let mut text = String::new();
    if !entry.blockers.is_empty() {
        text += &format!(", blocked by {}", entry.blockers.join(", "));
    }
    if entry.waits_for_update && entry.state == QueueState::Queued {
        text += ", after an update";
    }
    if let Some(held) = (entry.held_by.as_ref()).filter(|_| entry.state == QueueState::Queued) {
        text += &format!(", held by {}", held_by_label(held));
    }
    text
}

/// A waiting entry's rank: its priority and tier, each with the work it
/// took that from and why, as in `priority urgent (from 7, its parent),
/// finishing (from 12, blocked on this)`, then the areas it shares, as in
/// `, sharing area/daemon: 12, 15; area/cli: 3`.
fn rank_label(rank: &QueueRank) -> String {
    let tier = match rank.tier {
        Tier::New => "new",
        Tier::Started => "started",
        Tier::Finishing => "finishing",
    };
    format!(
        "priority {}{}, {tier}{}{}",
        rank.priority,
        inherited_label(rank.priority_from.as_ref()),
        inherited_label(rank.tier_from.as_ref()),
        shared_areas_label(&rank.shared_areas)
    )
}

/// The areas a launch shares, each with the spaces holding it, as in
/// `, sharing area/daemon: 12, 15; area/cli: 3`; nothing when it shares
/// none.
fn shared_areas_label(shared: &[SharedArea]) -> String {
    if shared.is_empty() {
        return String::new();
    }
    let areas: Vec<String> = (shared.iter())
        .map(|shared| format!("{}: {}", shared.area, shared.spaces.join(", ")))
        .collect();
    format!(", sharing {}", areas.join("; "))
}

/// The work a rank was taken from and why, as in ` (from 12, blocked on
/// this)`; nothing for a rank of the entry's own.
fn inherited_label(inherited: Option<&InheritedRank>) -> String {
    let Some(inherited) = inherited else {
        return String::new();
    };
    let why = match inherited.dependency {
        Dependency::Blocked => "blocked on this",
        Dependency::Parent => "its parent",
        Dependency::Question => "its child, asking it",
        Dependency::Stacked => "stacked on this",
        Dependency::Lease => "waiting for its lease",
        Dependency::Serves => "which it serves",
    };
    format!(" (from {}, {why})", inherited.from)
}

/// What holds a waiting entry back: `memory: needs 4.0 GiB, 1.2 GiB left`,
/// a limit's setting, low disk space, a failed reading, `CPU: 97% busy, 95%
/// level` or its tool's upstream limit.
pub fn held_by_label(held: &HeldBy) -> String {
    match *held {
        HeldBy::Memory { needed, left, .. } => format!(
            "memory: needs {}, {} left",
            size_label(needed),
            size_label(left)
        ),
        HeldBy::MemoryKept => "memory kept for a higher-ranked entry".into(),
        HeldBy::Disk => "low disk space".into(),
        HeldBy::MaxHeavyAgents => "machine.max_heavy_agents".into(),
        HeldBy::MaxJobs => "machine.max_jobs".into(),
        HeldBy::RepositoryLimit => "agent.max_heavy_running".into(),
        HeldBy::Permits => "runner.permit pool".into(),
        HeldBy::Reading => "critical or missing reading".into(),
        HeldBy::Cpu { busy, level } => format!("CPU: {busy}% busy, {level}% level"),
        HeldBy::UpstreamLimit { limit, until } => {
            let limit = match limit {
                ServiceLimit::RateLimited => "rate limited",
                ServiceLimit::Exhausted => "out of quota",
            };
            format!("upstream {limit} until {}", until.local_minutes())
        }
    }
}

/// What a queue entry starts: a launch's issue title, a restore, or a CI job.
pub(super) fn queue_subject(entry: &QueueEntry) -> String {
    match (entry.kind, &entry.job) {
        (QueueKind::Launch, _) => entry.issue_title.clone().unwrap_or_default(),
        (QueueKind::Restore, _) => format!("Restore of {}", entry.agent),
        (QueueKind::Job, Some(job)) => job_subject(job),
        (QueueKind::Job, None) => "CI job".into(),
    }
}

/// `CI job ci.yml / rust for PR #12`, naming the workflow where the forge
/// reports it and the PR or branch the run is for.
pub fn job_subject(queued: &crate::model::QueuedJob) -> String {
    use crate::forge::ci::runner::Trigger;
    let mut subject = match &queued.run.workflow {
        Some(workflow) => format!("CI job {workflow} / {}", queued.job.name),
        None => format!("CI job {}", queued.job.name),
    };
    match &queued.run.trigger {
        Trigger::Pull { number } => subject += &format!(" for PR #{number}"),
        Trigger::Branch(branch) => subject += &format!(" on {branch}"),
        Trigger::Other => {}
    }
    subject
}

/// One aligned row per agent queue entry: its workspace name, its repository
/// when the entries span several, its mode, its place, and what it starts.
pub fn queue_rows(
    entries: &[QueueEntry],
    repositories: &[Repository],
    palette: Palette,
) -> Vec<String> {
    let names: Vec<&str> = entries
        .iter()
        .map(|entry| {
            repositories
                .iter()
                .find(|repo| entry.repository_id.as_ref() == Some(&repo.id))
                .map_or("unknown repository", crate::repository::name)
        })
        .collect();
    let several = names.iter().collect::<HashSet<_>>().len() > 1;
    let cells: Vec<[Cell; 5]> = entries
        .iter()
        .zip(names)
        .map(|(entry, repository)| {
            [
                (entry.name.to_string(), Some(Style::Heading)),
                (
                    if several {
                        repository.into()
                    } else {
                        String::new()
                    },
                    None,
                ),
                mode_cell(entry.mode.as_ref(), &ModeName::DEFAULT),
                queue_status_cell(entry),
                (queue_subject(entry), None),
            ]
        })
        .collect();
    entries
        .iter()
        .zip(aligned(cells, palette))
        .map(|(entry, row)| format!("{} {row}", ListStatus::of_entry(entry).marker(palette)))
        .collect()
}

#[cfg(test)]
mod tests;
