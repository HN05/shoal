//! Combine native process evidence for one execution. Callers decide whether
//! uncertainty permits repair, ordinary stopping, or explicit manual removal.
use anyhow::Result;

use crate::{model::Execution, process::identity as process};

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
        let mut unreadable = Vec::new();
        for identity in &scan.unreadable {
            if execution
                .wrapper
                .as_ref()
                .is_none_or(|w| identity.not_older_than(w))
                && process::alive(identity)?
            {
                unreadable.push(identity.clone());
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
mod tests {
    use super::*;

    #[tokio::test]
    async fn unreadable_evidence_blocks_only_while_the_same_process_is_alive() {
        let mut child = tokio::process::Command::new("sleep")
            .arg("60")
            .env_clear()
            .env("SHOAL_TEST_PROCESS", "1")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let identity = process::capture(child.id().unwrap()).unwrap().unwrap();
        let execution = Execution {
            id: "test-execution".into(),
            workspace_id: "test-workspace".into(),
            state: crate::state::ExecutionState::Running,
            wrapper: Some(identity.clone()),
            child: None,
            group_id: Some(identity.pid),
        };
        let scan = process::Scan {
            unreadable: vec![identity],
            ..Default::default()
        };
        let live = Processes::inspect(&execution, &scan).await.unwrap();
        assert_eq!(live.unreadable.len(), 1);
        assert!(!live.visibility_complete());
        assert!(live.completion_issue().unwrap().contains(&format!(
            "unreadable live process environments (PID {})",
            child.id().unwrap()
        )));
        child.kill().await.unwrap();
        child.wait().await.unwrap();
        let exited = Processes::inspect(&execution, &scan).await.unwrap();
        assert!(exited.unreadable.is_empty());
        assert!(exited.visibility_complete());
        assert!(exited.completion_issue().is_none());
    }
}
