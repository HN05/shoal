//! Issue and PR actions the daemon carries out for a workspace, so its
//! scope rules apply and each change reaches the event journal.
use anyhow::{Context, Result, bail};

use super::{
    ForgeRepo,
    account::Account,
    action::Action,
    create::{NewIssue, NewPull, PullOptions},
    item::{Item, Opened},
    link::ItemKind,
    repository,
};
use crate::{
    daemon::{events, workspace::Manager},
    model::Workspace,
};

impl Manager {
    /// Apply `action` to the item `input` names in the workspace's repository,
    /// or to its linked item of `kind`.
    pub async fn item_action(
        &self,
        selector: &str,
        kind: ItemKind,
        input: Option<String>,
        action: Action,
    ) -> Result<Item> {
        action.validate(kind)?;
        let workspace = self.workspace(selector).await?;
        self.verify_worktree(&workspace).await?;
        let forge = workspace_forge(&workspace).await?;
        let input = match input {
            Some(input) => input,
            None => linked(&workspace, kind)?,
        };
        let (number, url) = match kind {
            ItemKind::Issue => forge.issue(&input)?,
            ItemKind::Pr => forge.pull(&input)?,
        };
        // Merging is the user's decision; every other change is the agent's.
        let account = match action {
            Action::Merge { .. } => Account::user(),
            _ => self.agent_account(&workspace).await?,
        };
        let item = forge
            .act(&account, kind, number, &action)
            .await
            .with_context(|| format!("could not {} {url}", action.name()))?;
        let id = workspace.id.clone();
        let name = action.name();
        self.store
            .run(move |db| events::record_item(db, &id, kind, &url, name))
            .await?;
        Ok(item)
    }
}

impl Manager {
    pub(super) async fn agent_account(&self, workspace: &Workspace) -> Result<Account> {
        let settings = self.workspace_settings(workspace).await?;
        Account::agent(&settings.agent_auth, &self.paths, &self.config().git)
    }
}

impl Manager {
    /// Open an issue in the workspace's repository as its agent account, and
    /// link it when `link` is set.
    pub async fn open_issue(&self, selector: &str, issue: NewIssue, link: bool) -> Result<Opened> {
        anyhow::ensure!(!issue.title.trim().is_empty(), "the issue needs a title");
        let workspace = self.workspace(selector).await?;
        self.verify_worktree(&workspace).await?;
        // Refuse before creating anything that could not then be linked.
        if link {
            anyhow::ensure!(
                workspace.links.issue.is_none(),
                "the workspace already has a linked issue; open the issue without --link"
            );
            anyhow::ensure!(
                workspace.state.accepts_issue(),
                "workspace cannot accept an issue in its current state"
            );
        }
        let forge = workspace_forge(&workspace).await?;
        let account = self.agent_account(&workspace).await?;
        let item = forge.create_issue(&account, &issue).await?;
        let (id, url) = (workspace.id.clone(), item.url.clone());
        self.store
            .run(move |db| events::record_item(db, &id, ItemKind::Issue, &url, "open"))
            .await?;
        if link {
            self.set_issue(&workspace.id, &item.url, Some(item.title.clone()))
                .await
                .with_context(|| format!("{} was opened, but linking it failed", item.url))?;
        }
        Ok(Opened {
            item,
            created: true,
            linked: link,
        })
    }
}

impl Manager {
    /// Link the open PR of the workspace's branch, or open one as its agent
    /// account and link it, so a PR Shoal opens is never left unlinked.
    pub async fn open_pull(&self, selector: &str, options: PullOptions) -> Result<Opened> {
        let workspace = self.workspace(selector).await?;
        self.verify_worktree(&workspace).await?;
        super::pr::current_head(&workspace).await?;
        let forge = workspace_forge(&workspace).await?;
        let account = self.agent_account(&workspace).await?;
        let existing = forge.open_pull_for(&account, &workspace.branch).await?;
        let (item, created) = match existing {
            Some(number) => (
                forge.read(&account, ItemKind::Pr, number).await?.item,
                false,
            ),
            None => {
                let pull = new_pull(&workspace, &forge, &account, options).await?;
                (forge.create_pull(&account, &pull).await?, true)
            }
        };
        if created {
            let (id, url) = (workspace.id.clone(), item.url.clone());
            self.store
                .run(move |db| events::record_item(db, &id, ItemKind::Pr, &url, "open"))
                .await?;
        }
        self.set_pr(
            &workspace.id,
            super::pr::Action::Watch {
                url: item.url.clone(),
            },
        )
        .await
        .with_context(|| format!("{} is open, but linking it failed", item.url))?;
        Ok(Opened {
            item,
            created,
            linked: true,
        })
    }
}

/// Fill what the caller left unset: the title from the linked issue, then
/// the last commit; a closing reference to the linked issue; the base
/// workspace's branch, then the default branch.
async fn new_pull(
    workspace: &Workspace,
    forge: &ForgeRepo,
    account: &Account,
    options: PullOptions,
) -> Result<NewPull> {
    let issue = match &workspace.links.issue {
        Some(url) => Some(forge.issue(url)?.0),
        None => None,
    };
    let title = match (options.title, &workspace.links.issue_title, issue) {
        (Some(title), _, _) => title,
        (None, Some(title), _) => title.clone(),
        (None, None, Some(number)) => {
            forge
                .read(account, ItemKind::Issue, number)
                .await?
                .item
                .title
        }
        (None, None, None) => crate::git::run(&workspace.path, &["log", "-1", "--format=%s"])
            .await?
            .trim()
            .to_owned(),
    };
    let base = match (options.base, &workspace.base_workspace) {
        (Some(base), _) => base,
        (None, Some(base)) => base.branch.clone(),
        (None, None) => {
            crate::git::default_branch::resolve(
                &workspace.path,
                crate::git::default_branch::DefaultBranchLookup::Discover,
            )
            .await?
        }
    };
    Ok(NewPull {
        title,
        body: description(options.body, issue),
        head: workspace.branch.clone(),
        base,
        draft: options.draft,
        labels: options.labels,
        reviewers: options.reviewers,
    })
}

/// A linked issue closes with the PR unless the description already says so.
fn description(body: Option<String>, issue: Option<u64>) -> String {
    let body = body.unwrap_or_default();
    let Some(number) = issue else {
        return body;
    };
    let closing = format!("Closes #{number}");
    if body.lines().any(|line| line.trim() == closing) {
        body
    } else if body.trim().is_empty() {
        closing
    } else {
        format!("{}\n\n{closing}", body.trim_end())
    }
}

/// The forge of the workspace's `origin`.
pub(super) async fn workspace_forge(workspace: &Workspace) -> Result<ForgeRepo> {
    let remote = repository::remote_url_from_path(&workspace.path)
        .await?
        .context("forge actions need an origin remote")?;
    ForgeRepo::parse(&remote)
}

fn linked(workspace: &Workspace, kind: ItemKind) -> Result<String> {
    match kind {
        ItemKind::Issue => workspace
            .links
            .issue
            .clone()
            .context("no issue is linked; pass a number or URL"),
        ItemKind::Pr => match workspace.links.prs.as_slice() {
            [url] => Ok(url.clone()),
            [] => bail!("no PR is linked; pass a number or URL"),
            _ => bail!("several PRs are linked; pass a number or URL"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linked_issue_adds_one_closing_line() {
        assert_eq!(description(None, None), "");
        assert_eq!(description(Some("Fix".into()), None), "Fix");
        assert_eq!(description(None, Some(7)), "Closes #7");
        assert_eq!(
            description(Some("Fix\n".into()), Some(7)),
            "Fix\n\nCloses #7"
        );
        assert_eq!(
            description(Some("Fix\n\nCloses #7\n".into()), Some(7)),
            "Fix\n\nCloses #7\n"
        );
        assert_eq!(
            description(Some("Closes #70".into()), Some(7)),
            "Closes #70\n\nCloses #7"
        );
    }
}
