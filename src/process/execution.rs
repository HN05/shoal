//! Combine native process evidence for one execution. Callers decide whether
//! uncertainty permits repair, ordinary stopping, or explicit manual removal.
use anyhow::Result;

use crate::{model::Execution, process::identity as process, protocol::timing};

pub struct Processes {
    pub wrapper: Option<process::Identity>,
    pub owned: Vec<process::Identity>,
    pub group_candidates: Vec<process::Identity>,
    pub unreadable: Vec<process::Identity>,
    pub launch_recorded: bool,
}

impl Processes {
    /// Reuse a marker scan across executions, but recheck wrapper/child identity
    /// and ancestry each time. After signaling, callers must obtain a fresh scan.
    pub async fn inspect(execution: &Execution, scan: &process::Scan) -> Result<Self> {
        let wrapper = match &execution.wrapper {
            Some(wrapper) if process::alive(wrapper)? => Some(wrapper.clone()),
            _ => None,
        };
        let mut owned: Vec<_> = scan
            .processes
            .iter()
            .filter(|p| p.execution_id == execution.id)
            .map(|p| p.identity.clone())
            .collect();
        let (related, group_candidates) = process::related(
            execution.child.as_ref(),
            execution.group_id,
            Some(&execution.id),
        )
        .await?;
        owned.extend(related);
        if let Some(child) = &execution.child
            && process::alive(child)?
        {
            owned.push(child.clone());
        }
        owned.sort_by(|a, b| a.pid.cmp(&b.pid).then(a.birth.cmp(&b.birth)));
        owned.dedup();
        let mut unreadable = Vec::new();
        let settled = std::time::Instant::now() + timing::PROCESS_SETTLE_TIMEOUT;
        for process in &scan.unreadable {
            if execution
                .wrapper
                .as_ref()
                .is_none_or(|w| process.head.not_older_than(w))
                && !owned.contains(&process.identity)
                && process::stays_unattributed(&process.identity, &execution.id, settled).await?
            {
                unreadable.push(process.identity.clone());
            }
        }
        Ok(Self {
            wrapper,
            owned,
            group_candidates,
            unreadable,
            launch_recorded: execution.wrapper.is_some() && execution.group_id.is_some(),
        })
    }

    /// Whether the scan found a process carrying the execution's marker
    /// outside its process group, such as a background command an agent
    /// started, whose completion may start its next turn. Neither the wrapper
    /// nor an agent hook waiting for the next update is one.
    pub fn runs_outside_group(execution: &Execution, scan: &process::Scan) -> bool {
        scan.processes.iter().any(|owned| {
            owned.execution_id == execution.id
                && execution.wrapper.as_ref() != Some(&owned.identity)
                && process::group_of(&owned.identity)
                    .is_some_and(|group| execution.group_id.is_none_or(|own| group != own))
                && !process::started_with(&owned.identity, crate::env::WAITING_HOOK)
        })
    }

    /// A marker can establish ownership even when the original group leader died.
    pub fn unverified(&self) -> Vec<process::Identity> {
        self.group_candidates
            .iter()
            .filter(|p| !self.owned.contains(p))
            .cloned()
            .collect()
    }

    pub fn has_survivors(&self) -> bool {
        self.wrapper.is_some() || !self.owned.is_empty() || !self.group_candidates.is_empty()
    }

    pub fn visibility_complete(&self) -> bool {
        self.launch_recorded && self.unreadable.is_empty()
    }

    /// A connected wrapper's exit report proves its command ended, including
    /// failure before launch. The wrapper stays alive awaiting acknowledgement;
    /// child/group survivors and incomplete environment visibility still block.
    pub fn command_stopped(&self) -> bool {
        self.owned.is_empty() && self.group_candidates.is_empty() && self.unreadable.is_empty()
    }

    /// Explain incomplete proof without disclosing process arguments or environments.
    pub fn completion_issue(&self) -> Option<String> {
        if self.command_stopped() {
            return None;
        }
        let unverified = self.unverified();
        let mut issues = Vec::new();
        for (label, processes) in [
            ("owned processes still running", self.owned.as_slice()),
            ("unverified process-group survivors", unverified.as_slice()),
            (
                "unreadable live process environments",
                self.unreadable.as_slice(),
            ),
        ] {
            if !processes.is_empty() {
                let pids: Vec<_> = processes.iter().map(|p| p.pid.to_string()).collect();
                issues.push(format!("{label} (PID {})", pids.join(", ")));
            }
        }
        Some(issues.join("; "))
    }

    /// Signal verified owned processes, leaving a live wrapper that still
    /// reports to the daemon.
    pub async fn stop_owned(&self) -> Result<()> {
        process::stop_verified(&self.owned).await
    }

    /// Only verified identities are signal targets. Return whether there were
    /// targets, not whether the execution can now be forgotten.
    pub async fn stop(&self) -> Result<bool> {
        let mut targets = self.owned.clone();
        targets.extend(self.wrapper.iter().cloned());
        process::stop_verified(&targets).await?;
        Ok(!targets.is_empty())
    }
}

#[cfg(test)]
mod tests;
