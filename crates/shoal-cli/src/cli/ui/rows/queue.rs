//! The agent queue's rows: each entry's place, what it waits for and what
//! it starts.
use super::*;

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

/// What a queue entry waits for, as its row's last status column:
/// `starting` once admitted, else the work that blocks it, an update and
/// what held it back at the last admission pass, as in `api, memory`.
fn waits_for_cell(entry: &QueueEntry) -> Cell {
    let status = ListStatus::of_entry(entry);
    if entry.state != QueueState::Queued {
        return (status.label().into(), Some(status.style()));
    }
    let mut waits = entry.blockers.clone();
    if entry.waits_for_update {
        waits.push("an update".into());
    }
    waits.extend(entry.held_by.as_ref().map(held_by_label));
    (waits.join(", "), Some(status.style()))
}

/// A waiting entry's priority; none once it starts.
fn priority_cell(entry: &QueueEntry) -> Cell {
    let rank = entry
        .rank
        .as_ref()
        .filter(|_| entry.state == QueueState::Queued);
    (
        rank.map(|rank| rank.priority.to_string())
            .unwrap_or_default(),
        None,
    )
}

/// What holds a waiting entry back: memory, whose detail `queue --json`
/// gives, a limit's setting, low disk space, a failed reading, `CPU: 97%
/// busy, 95% level` or its tool's upstream limit.
pub fn held_by_label(held: &HeldBy) -> String {
    match *held {
        HeldBy::Memory { .. } => "memory".into(),
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

/// The agent queue as a table: a header naming its columns, then one row
/// per entry.
pub struct QueueTable {
    pub header: String,
    pub rows: Vec<String>,
}

/// The queue's `entries` as a table, each row led by the entry's place in
/// the queue, the number of its space, which `shoal queue start` and
/// `cancel` take, and its marker, then its workspace name, its repository
/// when the entries span several, its mode, its priority, what it waits for
/// and what it starts.
pub fn queue_rows(
    entries: &[QueueEntry],
    repositories: &[Repository],
    palette: Palette,
) -> QueueTable {
    let names = named_repository_cells(entries.iter().map(|entry| {
        let repository = repositories
            .iter()
            .find(|repo| entry.repository_id.as_ref() == Some(&repo.id));
        repository.map(crate::repository::name)
    }));
    let cells: Vec<[Cell; 6]> = (entries.iter().zip(names))
        .map(|(entry, repository)| entry_cells(entry, repository))
        .collect();
    let header = header_cells(&cells);
    let mut lines = aligned([header].into_iter().chain(cells).collect(), palette).into_iter();
    let header = lines.next().unwrap_or_default();
    placed(entries, header, lines.collect(), palette)
}

/// One entry's cells after its place, number and marker.
fn entry_cells(entry: &QueueEntry, repository: String) -> [Cell; 6] {
    [
        (entry.name.to_string(), Some(Style::Heading)),
        (repository, None),
        mode_cell(entry.mode.as_ref(), &ModeName::DEFAULT),
        priority_cell(entry),
        waits_for_cell(entry),
        (queue_subject(entry), None),
    ]
}

/// The queue table's column titles, each only over a column some row
/// fills.
fn header_cells(rows: &[[Cell; 6]]) -> [Cell; 6] {
    let titles = ["NAME", "REPOSITORY", "MODE", "PRIORITY", "WAITS FOR", ""];
    let mut column = 0;
    titles.map(|title| {
        let filled = rows.iter().any(|row| !row[column].0.is_empty());
        column += 1;
        let title = if filled { title } else { "" };
        (title.into(), Some(Style::Muted))
    })
}

/// The aligned `header` and `rows`, each row led by its entry's place in
/// the queue and its space's number, each right aligned under its title,
/// and its marker.
fn placed(
    entries: &[QueueEntry],
    header: String,
    rows: Vec<String>,
    palette: Palette,
) -> QueueTable {
    let places = entries
        .iter()
        .map(|entry| entry.position.map(|place| place.to_string()));
    let (place_title, places) = right_aligned("#", places, palette);
    let numbers = entries
        .iter()
        .map(|entry| entry.number.map(|number| number.to_string()));
    let (number_title, numbers) = right_aligned("ID", numbers, palette);
    let rows = (entries.iter().zip(places).zip(numbers).zip(rows))
        .map(|(((entry, place), number), row)| {
            let marker = ListStatus::of_entry(entry).marker(palette);
            format!("{place}  {number} {marker} {row}")
        })
        .collect();
    QueueTable {
        header: format!("{place_title}  {number_title}   {header}"),
        rows,
    }
}

/// A column of `values` right aligned under `title`: the title as the
/// header shows it, then each value, empty where there is none.
fn right_aligned(
    title: &str,
    values: impl Iterator<Item = Option<String>>,
    palette: Palette,
) -> (String, Vec<String>) {
    let values: Vec<String> = values.map(Option::unwrap_or_default).collect();
    let width = values.iter().map(String::len).chain([title.len()]).max();
    let width = width.unwrap_or_default();
    let title = palette.paint(Style::Muted, format!("{title:>width$}"));
    let values = values
        .iter()
        .map(|value| format!("{value:>width$}"))
        .collect();
    (title, values)
}

#[cfg(test)]
mod tests;
