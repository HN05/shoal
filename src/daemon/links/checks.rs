//! Failed Actions logs resolved from a PR's current check, using its agent account.
use anyhow::Result;

use crate::{
    daemon::workspace::Manager,
    forge::{
        ci::checks::{CheckLog, validate_check_name},
        items::link::{ItemKind, Selection},
        updates::Update,
    },
};

impl Manager {
    /// Log failures stay on their CI update and never hide other activity.
    pub(crate) async fn summarize_failed_checks(&self, workspace: &str, updates: &mut [Update]) {
        for index in 0..updates.len() {
            let update = &updates[index];
            let Some(check) = &update.check else { continue };
            if !matches!(check.result.as_str(), "failure" | "error")
                || !self
                    .pull_forge(&update.url)
                    .await
                    .is_ok_and(|forge| forge.supports_check_logs())
            {
                continue;
            }
            let selection = Selection {
                kind: Some(ItemKind::Pr),
                input: Some(update.url.clone()),
            };
            let log = self.check_log(workspace, &selection, &check.name).await;
            let summary = match log {
                Ok(log) => log
                    .failed_step
                    .map(|step| format!("; failed step: {step}"))
                    .unwrap_or_default(),
                Err(error) => format!(
                    "; log unavailable: {}",
                    format!("{error:#}").chars().take(512).collect::<String>()
                ),
            };
            let message = format!(
                "{}{summary}; read the log with shoal view {} --check '{}'",
                update.message,
                update.url,
                check.name.replace('\'', "'\\''")
            );
            let previous = std::mem::replace(&mut updates[index].message, message);
            if serde_json::to_vec(updates).map_or(true, |bytes| {
                bytes.len() >= crate::protocol::MAX_FRAME - 1024
            }) {
                updates[index].message = previous;
            }
        }
    }

    pub async fn check_log(
        &self,
        selector: &str,
        selection: &Selection,
        name: &str,
    ) -> Result<CheckLog> {
        validate_check_name(name)?;
        let workspace = self.workspace(selector).await?;
        self.verify_worktree(&workspace).await?;
        let selection = Selection {
            kind: selection.kind.or(Some(ItemKind::Pr)),
            input: selection.input.clone(),
        };
        let items = self.selected_items(&workspace, &selection).await?;
        let [(url, ItemKind::Pr)] = items.as_slice() else {
            anyhow::bail!(
                "select one PR to read a check log; use shoal view pr <number> --check <name>"
            );
        };
        let forge = self.workspace_forge(&workspace).await?;
        let account = self.agent_account(&workspace).await?;
        forge.failed_check_log(&account, url, name).await
    }
}

#[cfg(test)]
mod tests {
    use crate::forge::ci::checks::{CheckLog, TAIL_BYTES};

    #[test]
    fn the_longest_log_tail_fits_one_protocol_frame_even_when_escaped() {
        let log = CheckLog {
            url: "pr".into(),
            name: "rust".into(),
            text: "\0".repeat(TAIL_BYTES),
            truncated: true,
            failed_step: Some("\0".repeat(256)),
        };
        assert!(serde_json::to_vec(&log).unwrap().len() < crate::protocol::MAX_FRAME);
    }
}
