//! Failed Actions logs resolved from a PR's current check, using its agent account.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{ForgeRepo, account::Account, backends::Backend, capability::Capability};

/// The most of a log's end a [`CheckLog`] keeps, in bytes.
pub const TAIL_BYTES: usize = 8192;
/// The longest job URL a [`CheckLog`] keeps, in bytes.
pub const JOB_URL_BYTES: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckLog {
    /// The PR's.
    pub url: String,
    pub name: String,
    /// The web page of the check's job, unless longer than [`JOB_URL_BYTES`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_url: Option<String>,
    pub text: String,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_step: Option<String>,
}

/// Refuse a check name no forge shows, before any lookup.
pub fn validate_check_name(name: &str) -> Result<()> {
    ensure!(
        !name.trim().is_empty() && name.len() <= 1024,
        "check name must be nonempty and at most 1024 bytes"
    );
    Ok(())
}

impl ForgeRepo {
    pub async fn failed_check_log(
        &self,
        account: &Account,
        url: &str,
        name: &str,
    ) -> Result<CheckLog> {
        self.require(Capability::ReadJobLogs)?;
        self.kind.failed_check_log(self, account, url, name).await
    }
}

impl CheckLog {
    pub(crate) fn from_text(url: &str, name: &str, text: &str) -> Self {
        let failed_step = failed_step(text);
        let mut start = text.len().saturating_sub(TAIL_BYTES);
        while !text.is_char_boundary(start) {
            start += 1;
        }
        Self {
            url: url.into(),
            name: name.into(),
            job_url: None,
            text: text[start..].into(),
            truncated: start > 0,
            failed_step,
        }
    }

    pub(crate) fn with_job(self, job_url: String) -> Self {
        Self {
            job_url: Some(job_url).filter(|url| url.len() <= JOB_URL_BYTES),
            ..self
        }
    }
}

fn failed_step(text: &str) -> Option<String> {
    let mut current = None;
    for line in text.lines() {
        if let Some((_, step)) = line.split_once("⭐ Run ") {
            current = Some(step.trim().chars().take(256).collect());
        }
        if let Some((_, step)) = line.split_once("❌  Failure - ") {
            return Some(step.trim().chars().take(256).collect());
        }
        if line.contains("[runner]:") && line.contains("failure") {
            return current;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tails_are_bounded_and_retain_step_before_long_output() {
        let text = format!(
            "⭐ Run Main Test\n{}\n⚙️ [runner]: exitcode '1': failure\nJob failed\n",
            "🦀".repeat(TAIL_BYTES)
        );
        let log = CheckLog::from_text("pr", "rust", &text);
        assert!(log.truncated);
        assert!(log.text.len() <= TAIL_BYTES);
        assert_eq!(log.failed_step.as_deref(), Some("Main Test"));
        assert!(log.text.ends_with("Job failed\n"));
        assert!(!CheckLog::from_text("pr", "rust", "short").truncated);
        let long = CheckLog::from_text("pr", "rust", &"\0".repeat(TAIL_BYTES * 2));
        assert_eq!(long.text.len(), TAIL_BYTES);
        let job = |length| CheckLog::from_text("pr", "rust", "").with_job("j".repeat(length));
        assert!(job(JOB_URL_BYTES).job_url.is_some());
        assert!(job(JOB_URL_BYTES + 1).job_url.is_none());
    }
}
