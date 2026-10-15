//! GitLab's REST shapes: issues and merge requests numbered by `iid`,
//! projects addressed by their encoded path, and states of its own.
use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::{
    ForgeRepo,
    item::{Current, Item, ItemState, PrSummary, text},
    link::ItemKind,
};

/// Merge statuses during which GitLab has not yet decided mergeability.
const UNDECIDED: [&str; 4] = ["unchecked", "checking", "preparing", "approvals_syncing"];

impl ForgeRepo {
    /// An issue or merge request as GitLab reports it.
    pub(super) fn gitlab_current(
        &self,
        kind: ItemKind,
        number: u64,
        value: &Value,
    ) -> Result<Current> {
        let state = match value["state"].as_str() {
            // A merge request is locked while GitLab merges it.
            Some("opened" | "locked") => ItemState::Open,
            Some("closed") => ItemState::Closed,
            Some("merged") if kind == ItemKind::Pr => ItemState::Merged,
            state => bail!("unrecognized GitLab state {state:?}"),
        };
        let pr = match kind {
            ItemKind::Issue => None,
            ItemKind::Pr => Some(PrSummary {
                head: text(&value["source_branch"])
                    .context("merge request has no source branch")?,
                base: text(&value["target_branch"])
                    .context("merge request has no target branch")?,
                draft: value["draft"]
                    .as_bool()
                    .or(value["work_in_progress"].as_bool())
                    .unwrap_or_default(),
            }),
        };
        let merge_status = value["detailed_merge_status"].as_str().unwrap_or_default();
        let conflict = match (kind, value["has_conflicts"].as_bool()) {
            (ItemKind::Pr, Some(conflicts)) if !UNDECIDED.contains(&merge_status) => {
                Some(conflicts)
            }
            _ => None,
        };
        let same_project = value["source_project_id"] == value["target_project_id"];
        Ok(Current {
            item: Item {
                kind,
                number,
                url: self.item_url(kind, number)?,
                title: text(&value["title"]).context("item has no title")?,
                state,
                labels: labels(&value["labels"]),
                pr,
            },
            node_id: None,
            head_repository: (kind == ItemKind::Pr && same_project).then(|| self.path.clone()),
            body: text(&value["description"]).unwrap_or_default(),
            head_sha: text(&value["sha"]),
            conflict,
        })
    }
}

/// Label names, listed as strings unless details were requested.
fn labels(labels: &Value) -> Vec<String> {
    labels
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|label| text(label).or_else(|| text(&label["name"])))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn gitlab() -> ForgeRepo {
        ForgeRepo::parse("git@gitlab.com:group/sub/project.git").unwrap()
    }

    #[test]
    fn merge_requests_read_branches_drafts_and_conflicts() {
        let merge_request = json!({"iid": 7, "title": "Draft: Fix", "state": "opened",
            "description": "Why", "labels": ["bug"], "source_branch": "topic",
            "target_branch": "main", "draft": true, "sha": "abc", "has_conflicts": true,
            "detailed_merge_status": "conflict", "source_project_id": 1, "target_project_id": 1});
        let current = gitlab().current(ItemKind::Pr, 7, &merge_request).unwrap();
        assert_eq!(
            current.item,
            Item {
                kind: ItemKind::Pr,
                number: 7,
                url: "https://gitlab.com/group/sub/project/-/merge_requests/7".into(),
                title: "Draft: Fix".into(),
                state: ItemState::Open,
                labels: vec!["bug".into()],
                pr: Some(PrSummary {
                    head: "topic".into(),
                    base: "main".into(),
                    draft: true,
                }),
            }
        );
        assert_eq!(current.conflict, Some(true));
        assert_eq!(current.head_sha.as_deref(), Some("abc"));
        assert_eq!(
            current.head_repository.as_deref(),
            Some("group/sub/project")
        );
        assert_eq!(current.body, "Why");

        let mut checking = merge_request.clone();
        checking["detailed_merge_status"] = json!("checking");
        checking["source_project_id"] = json!(2);
        let current = gitlab().current(ItemKind::Pr, 7, &checking).unwrap();
        assert_eq!(current.conflict, None);
        assert_eq!(current.head_repository, None);

        let mut merged = merge_request;
        merged["state"] = json!("merged");
        let current = gitlab().current(ItemKind::Pr, 7, &merged).unwrap();
        assert_eq!(current.item.state, ItemState::Merged);
        assert!(gitlab().current(ItemKind::Pr, 8, &merged).is_err());
    }

    #[test]
    fn issues_use_their_own_numbers_routes_and_states() {
        let issue = json!({"iid": 3, "id": 900, "title": "Bug", "state": "closed",
            "labels": [{"name": "type/bug"}]});
        let current = gitlab().current(ItemKind::Issue, 3, &issue).unwrap();
        assert_eq!(
            current.item.url,
            "https://gitlab.com/group/sub/project/-/issues/3"
        );
        assert_eq!(current.item.state, ItemState::Closed);
        assert_eq!(current.item.labels, ["type/bug"]);
        assert!(gitlab().current(ItemKind::Issue, 900, &issue).is_err());
        let mut merged = issue;
        merged["state"] = json!("merged");
        assert!(gitlab().current(ItemKind::Issue, 3, &merged).is_err());
    }
}
