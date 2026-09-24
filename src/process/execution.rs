//! Combine native process evidence for one execution. Callers decide whether
//! uncertainty permits repair, ordinary stopping, or explicit manual removal.
use anyhow::Result;

use crate::{model::Execution, process::identity as process};

pub struct Processes {
    pub wrapper: Option<process::Identity>,
    pub owned: Vec<process::Identity>,
    pub group_candidates: Vec<process::Identity>,
    pub unreadable: usize,
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
        let (related, group_candidates) =
            process::related(execution.child.as_ref(), execution.group_id).await?;
        owned.extend(related);
        if let Some(child) = &execution.child
            && process::alive(child)?
        {
            owned.push(child.clone());
        }
        owned.sort_by(|a, b| a.pid.cmp(&b.pid).then(a.birth.cmp(&b.birth)));
        owned.dedup();
        let unreadable = scan
            .unreadable
            .iter()
            .filter(|p| {
                execution
                    .wrapper
                    .as_ref()
                    .is_none_or(|w| p.not_older_than(w))
            })
            .count();
        Ok(Self {
            wrapper,
            owned,
            group_candidates,
            unreadable,
            launch_recorded: execution.wrapper.is_some() && execution.group_id.is_some(),
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
        self.launch_recorded && self.unreadable == 0
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
