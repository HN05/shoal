use anyhow::{Context, Result};

use super::Manager;
use crate::{
    daemon::store,
    git,
    model::{DiffSummary, WorkspaceStatus},
};

impl Manager {
    pub async fn workspace_status(&self, selector: &str) -> Result<WorkspaceStatus> {
        let mut inspection = self.inspect_workspace(selector).await?;
        let diff = async {
            self.verify_worktree(&inspection.workspace).await?;
            let base = self.diff_base(&inspection.workspace.id).await?;
            let numstat = git::run(
                &inspection.workspace.path,
                &["diff", "--numstat", &base.commit, "--"],
            )
            .await?;
            parse_numstat(&numstat)
        }
        .await;
        let (diff, diff_error) = match diff {
            Ok(diff) => (Some(diff), None),
            Err(error) => (None, Some(format!("{error:#}"))),
        };
        let unread_notifications = self.unread_notifications().await?;
        let workspace_id = inspection.workspace.id.clone();
        let setup_finished = self
            .store
            .run(move |db| store::setup_finished(db, &workspace_id))
            .await?;
        inspection.simulators.retain(|simulator| {
            simulator.workspace_id.as_deref() == Some(inspection.workspace.id.as_str())
        });
        Ok(WorkspaceStatus {
            inspection,
            setup_finished,
            diff,
            diff_error,
            unread_notifications,
        })
    }
}

fn parse_numstat(output: &str) -> Result<DiffSummary> {
    let mut summary = DiffSummary {
        files_changed: 0,
        insertions: 0,
        deletions: 0,
    };
    for line in output.lines() {
        let mut fields = line.splitn(3, '\t');
        let insertions = fields.next().context("Git numstat omitted insertions")?;
        let deletions = fields.next().context("Git numstat omitted deletions")?;
        fields.next().context("Git numstat omitted the file name")?;
        summary.files_changed += 1;
        if insertions != "-" {
            summary.insertions += insertions
                .parse::<u64>()
                .context("invalid Git insertions")?;
        }
        if deletions != "-" {
            summary.deletions += deletions.parse::<u64>().context("invalid Git deletions")?;
        }
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numstat_counts_text_and_binary_files() {
        let summary = parse_numstat("2\t1\tfirst\n10\t0\tsecond\n-\t-\timage.png\n").unwrap();
        assert_eq!(summary.files_changed, 3);
        assert_eq!(summary.insertions, 12);
        assert_eq!(summary.deletions, 1);
    }

    #[test]
    fn numstat_rejects_incomplete_records() {
        assert!(parse_numstat("1\t2\n").is_err());
    }
}
