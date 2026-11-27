//! What a start and the queue's lists say of work waiting in the agent
//! queue, and of the memory it waits for.
use shoal_protocol::{
    executions::ServiceLimit,
    model::{HeldBy, QueueEntry},
    state::QueueState,
};

/// When the agent of `entry` starts, as a list or a start says it.
pub fn starts_when(entry: &QueueEntry) -> String {
    match entry.blockers.as_slice() {
        [] => "when a heavy slot is free".to_owned(),
        blockers => format!(
            "once it is no longer blocked by {} and a heavy slot is free",
            blockers.join(", ")
        ),
    }
}

/// What a start says of the agent of `entry`, queued in the space `name`.
pub fn queued_in(name: &str, entry: &QueueEntry) -> String {
    match entry.state {
        QueueState::Queued => format!(
            "Queued {} in {name} (position {}); it starts {}",
            entry.agent,
            entry.position.unwrap_or(1),
            starts_when(entry)
        ),
        _ => format!("Starting {} in {name}", entry.agent),
    }
}

/// What holds a waiting entry back: `memory: needs 4.0 GiB, 1.2 GiB left`,
/// `memory kept for api-tests`, a limit's setting, low disk space, a failed
/// reading, `CPU: 97% busy, 95% level` or its tool's upstream limit.
pub fn held_by_label(held: &HeldBy) -> String {
    match *held {
        HeldBy::Memory { needed, left } => format!(
            "memory: needs {}, {} left",
            size_label(needed),
            size_label(left)
        ),
        HeldBy::MemoryKept { ref behind } => format!("memory kept for {behind}"),
        HeldBy::Disk => "low disk space".into(),
        HeldBy::MaxHeavyAgents => "machine.max_heavy_agents".into(),
        HeldBy::MaxJobs => "machine.max_jobs".into(),
        HeldBy::RepositoryLimit => "agent.max_heavy_running".into(),
        HeldBy::Permits => "runner.permit pool".into(),
        HeldBy::RankedBehind { ahead: 1 } => "1 machine ranks ahead".into(),
        HeldBy::RankedBehind { ahead } => format!("{ahead} machines rank ahead"),
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

/// A size in bytes as lists show it: `512 B`, `12 KiB`, `640 MiB` or `1.5 GiB`.
pub fn size_label(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    let bytes = bytes as f64;
    if bytes < KIB {
        format!("{bytes} B")
    } else if bytes < KIB * KIB {
        format!("{:.0} KiB", bytes / KIB)
    } else if bytes < KIB * KIB * KIB {
        format!("{:.0} MiB", bytes / KIB / KIB)
    } else {
        format!("{:.1} GiB", bytes / KIB / KIB / KIB)
    }
}
