//! Issue and PR actions the daemon carries out for a workspace, so its
//! scope rules apply and each change reaches the event journal.
use anyhow::{Context, Result, bail};

use super::{
    proposals::{OpenOutcome, Proposer},
    wait::{OwnChange, record_own_change},
};
use crate::{
    config::{
        Config,
        repo::{Opening, OtherRepositories},
        resolve::Effective,
    },
    daemon::{events, workspace::Manager},
    forge::{
        ForgeRepo,
        account::{Account, AgentLogins},
        conventions,
        items::{
            action::{Action, Edit, Sent},
            create::{Created, NewIssue, NewPull, PullOptions},
            item::{Item, ItemState, Opened},
            link::ItemKind,
        },
        transport::memory::Memory,
    },
    fsutil::expand_home,
    model::{Relation, ReviewMark, Workspace},
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
        let forge = self.workspace_forge(&workspace).await?;
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
            Action::Merge { .. } => self.user_account(),
            _ => self.agent_account(&workspace).await?,
        };
        // Held from before the change, so a poll that observes it records
        // it after the workspace's cursor knows it as its own.
        let _gate = self.pr_gates.lock([&workspace.id]).await;
        let target = Target { kind, number, url };
        self.apply_own(&workspace.id, &forge, &account, target, &action)
            .await
    }

    /// Move the workspace's open PRs at `urls` into or out of draft as its
    /// agent account, leaving those already in that state unchanged. The
    /// caller holds the workspace's PR gate.
    pub(crate) async fn set_draft(
        &self,
        workspace: &Workspace,
        urls: &[String],
        draft: bool,
    ) -> Result<()> {
        if urls.is_empty() {
            return Ok(());
        }
        let forge = self.workspace_forge(workspace).await?;
        let account = self.agent_account(workspace).await?;
        let action = Action::Edit(Edit {
            draft: Some(draft),
            ..Edit::default()
        });
        for url in urls {
            let (number, url) = forge.pull(url)?;
            let item = forge.read(&account, ItemKind::Pr, number).await?.item;
            let pending = item.state == ItemState::Open
                && item.pr.as_ref().is_some_and(|pr| pr.draft != draft);
            if pending {
                let target = Target {
                    kind: ItemKind::Pr,
                    number,
                    url,
                };
                self.apply_own(&workspace.id, &forge, &account, target, &action)
                    .await?;
            }
        }
        Ok(())
    }

    /// Add to each mark what its item leaves out of the repository's
    /// conventions, read as the workspace's agent account. Its description
    /// was checked when Shoal opened it, so only its labels are.
    pub(crate) async fn check_conventions(
        &self,
        workspace: &Workspace,
        settings: &Effective,
        marks: &mut [ReviewMark],
    ) {
        let Ok((forge, account)) = self.workspace_forge_account(workspace).await else {
            return;
        };
        for mark in marks {
            let Some(item) = marked_item(&forge, &account, mark).await else {
                continue;
            };
            let required = required_labels(settings, item.kind);
            mark.warnings =
                conventions::warnings(&forge, &account, &workspace.path, &item, None, required)
                    .await;
        }
    }

    /// The forge of the workspace's `origin`.
    pub(super) async fn workspace_forge(&self, workspace: &Workspace) -> Result<ForgeRepo> {
        let remote = crate::repository::remote_url_from_path(&workspace.path)
            .await?
            .context("forge actions need an origin remote")?;
        self.forge(&remote).await
    }

    pub(super) async fn workspace_forge_account(
        &self,
        workspace: &Workspace,
    ) -> Result<(ForgeRepo, Account)> {
        Ok((
            self.workspace_forge(workspace).await?,
            self.agent_account(workspace).await?,
        ))
    }

    /// Apply `action` and record it as the workspace's own change, as well as
    /// a comment it posted before failing. The caller holds the workspace's
    /// PR gate.
    pub(super) async fn apply_own(
        &self,
        id: &str,
        forge: &ForgeRepo,
        account: &Account,
        target: Target,
        action: &Action,
    ) -> Result<Item> {
        let acted = match forge
            .apply(account, target.kind, target.number, action)
            .await
        {
            Ok(acted) => acted,
            Err(error) => {
                let error = error.context(format!("could not {} {}", action.name(), target.url));
                return Err(self.record_failed_post(id, target, action, error).await);
            }
        };
        let own = own_state(action).then(|| acted.item.state.as_str());
        self.record_own_action(id, target, action.name(), own, acted.authored)
            .await
            .map_err(|error| crate::forge::items::action::after_send(error, true))?;
        Ok(acted.item)
    }

    /// Record the comment or reply a failed action posted as the workspace's
    /// own, so polling does not report it back, and return the failure.
    async fn record_failed_post(
        &self,
        id: &str,
        target: Target,
        action: &Action,
        error: anyhow::Error,
    ) -> anyhow::Error {
        let Some(authored) = error
            .downcast_ref::<Sent>()
            .and_then(|sent| sent.authored.clone())
        else {
            return error;
        };
        match self
            .record_own_action(id, target, action.name(), None, Some(authored))
            .await
        {
            Ok(()) => error,
            Err(recording) => error.context(format!(
                "recording what was posted also failed: {recording:#}"
            )),
        }
    }

    /// Journal an applied action and advance the workspace's cursor past it,
    /// so polling does not report the change back as someone else's.
    async fn record_own_action(
        &self,
        id: &str,
        target: Target,
        name: &'static str,
        state: Option<&'static str>,
        authored: Option<String>,
    ) -> Result<()> {
        let id = id.to_owned();
        let Target { kind, url, .. } = target;
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                events::record_item(&tx, &id, kind, &url, name)?;
                let change = OwnChange {
                    url: &url,
                    authored,
                    state,
                };
                record_own_change(&tx, &id, change)?;
                tx.commit()?;
                Ok(())
            })
            .await
    }
}

/// The forge memory of the user's own logins.
const USER_LOGINS: &str = "user";

/// The item an action changes.
#[derive(Clone)]
pub(super) struct Target {
    pub kind: ItemKind,
    pub number: u64,
    pub url: String,
}

/// Whether the state an action leaves is the agent's own change. Merging is
/// the user's decision, which the agent still hears about.
fn own_state(action: &Action) -> bool {
    matches!(action, Action::Close | Action::Reopen)
}

impl Manager {
    /// The user's own logins, which reads and the user's decisions use.
    pub(crate) fn user_account(&self) -> Account {
        Account::user(&self.config().bitbucket).remembering(self.forge_memory(USER_LOGINS))
    }

    pub(super) async fn agent_account(&self, workspace: &Workspace) -> Result<Account> {
        // One snapshot, so a reload cannot pair a profile with another map.
        let config = self.config();
        let settings = self.workspace_settings_from(&config, workspace).await?;
        self.agent_account_from(&config, &settings)
    }

    /// The agent account `settings` select, with the Git profiles of
    /// `config`, the snapshot they were resolved from.
    pub(super) fn agent_account_from(
        &self,
        config: &Config,
        settings: &Effective,
    ) -> Result<Account> {
        let auth = &settings.agent_auth;
        let logins = AgentLogins {
            launch: Box::new(auth.prepare(&self.paths, &config.git)?),
            fj_home: (auth.fj_home.as_ref()).map(|home| expand_home(home, &self.paths.home)),
            fj: auth.fj.is_some(),
            tea: auth.tea.is_some(),
        };
        // An agent with the user's logins shares what the user's requests taught.
        let identity = match auth.forge_identity()? {
            Some(logins) => format!("agent {logins}"),
            None => USER_LOGINS.to_owned(),
        };
        Ok(Account::agent(logins, &config.bitbucket).remembering(self.forge_memory(&identity)))
    }

    /// The memory of forge responses to the account `identity` names.
    fn forge_memory(&self, identity: &str) -> Memory {
        let mut memories =
            (self.forge_memories.lock()).unwrap_or_else(|poisoned| poisoned.into_inner());
        memories.entry(identity.to_owned()).or_default().clone()
    }
}

impl Manager {
    /// Open an issue as the workspace's agent account in its repository, or
    /// the one `repository` names, and link it when `link` is set. A
    /// `proposer` records it for the user's approval instead when
    /// `[approvals] issue_open`, or for another repository
    /// `[issue] other_repositories`, says so.
    pub async fn open_issue(
        &self,
        selector: &str,
        issue: NewIssue,
        repository: Option<String>,
        link: bool,
        proposer: Option<Proposer>,
    ) -> Result<OpenOutcome> {
        anyhow::ensure!(!issue.title.trim().is_empty(), "the issue needs a title");
        // Cleanup holds the workspace's gate, so it cannot remove the
        // workspace between opening the issue and linking it.
        let (workspace, _gate) = self.lock_pr_gate(selector).await?;
        self.verify_worktree(&workspace).await?;
        // Refuse before creating anything that could not then be recorded
        // or linked.
        anyhow::ensure!(
            workspace.state.accepts_links(),
            "workspace cannot accept an issue in its current state"
        );
        anyhow::ensure!(
            !link || workspace.links.issue.is_none(),
            "the workspace already has a linked issue; open the issue without --link"
        );
        let settings = self.workspace_settings(&workspace).await?;
        let (own, account) = self.workspace_forge_account(&workspace).await?;
        let destination = self.issue_destination(own, repository.as_deref()).await?;
        anyhow::ensure!(
            !link || !destination.elsewhere,
            "an issue in another repository cannot be linked; open it without --link"
        );
        let opening = match proposer {
            Some(_) => destination.opening(&settings)?,
            None => Opening::Direct,
        };
        let (forge, required) = (&destination.forge, destination.required(&settings));
        conventions::require(forge, &account, ItemKind::Issue, required, &issue.labels).await?;
        if let Some(proposer) = proposer.filter(|_| opening == Opening::Propose) {
            forge.check_uploads(&account, &issue.attachments).await?;
            let repository = destination.repository();
            let proposed = self
                .propose_issue(&workspace, issue, repository, link, proposer)
                .await?;
            return Ok(OpenOutcome::Proposed(proposed));
        }
        let Created { item, unfinished } = forge.create_issue(&account, &issue).await?;
        let (linked, warning) = self
            .keep_created(&workspace, ItemKind::Issue, &item, link)
            .await?;
        // The workspace's conventions do not apply to another repository,
        // and its item commands cannot change an issue there.
        let checked = if destination.elsewhere {
            Vec::new()
        } else {
            let body = Some(issue.body.as_str());
            conventions::warnings(forge, &account, &workspace.path, &item, body, required).await
        };
        Ok(OpenOutcome::Opened(Opened {
            item,
            created: true,
            linked,
            warnings: warning
                .into_iter()
                .chain(unfinished)
                .chain(checked)
                .collect(),
        }))
    }

    /// The repository `named` names for a new issue, or the workspace's
    /// repository `own` without one.
    async fn issue_destination(
        &self,
        own: ForgeRepo,
        named: Option<&str>,
    ) -> Result<IssueDestination> {
        let Some(named) = named else {
            return Ok(IssueDestination {
                forge: own,
                elsewhere: false,
            });
        };
        let forge = own.resolve_named(named, &self.forge_hosts()).await?;
        let elsewhere = forge != own;
        Ok(IssueDestination { forge, elsewhere })
    }
}

/// The repository a new issue opens in, and whether it is another than the
/// workspace's, whose settings and conventions then do not apply.
struct IssueDestination {
    forge: ForgeRepo,
    elsewhere: bool,
}

impl IssueDestination {
    /// How a scoped caller opens the issue here under `settings`, refusing
    /// another repository they deny.
    fn opening(&self, settings: &Effective) -> Result<Opening> {
        if !self.elsewhere {
            return Ok(settings.approvals.issue_open);
        }
        match settings.issue.other_repositories {
            OtherRepositories::Deny => bail!(
                "agents cannot open issues in {}: [issue] other_repositories is \"deny\"",
                self.forge.repository_url()
            ),
            OtherRepositories::Propose => Ok(Opening::Propose),
            OtherRepositories::Direct => Ok(Opening::Direct),
        }
    }

    /// The labels `settings` require of the new issue; none elsewhere.
    fn required<'a>(&self, settings: &'a Effective) -> &'a [String] {
        if self.elsewhere {
            &[]
        } else {
            &settings.issue.required_labels
        }
    }

    /// The URL a proposal records for another repository.
    fn repository(&self) -> Option<String> {
        self.elsewhere.then(|| self.forge.repository_url())
    }
}

impl Manager {
    /// Link the open PR of the workspace's branch, or open one as its agent
    /// account and link it; only a removal that starts meanwhile leaves it
    /// unlinked. A `proposer` records a new PR for the user's approval
    /// instead when `[approvals] pr_open` says so.
    pub async fn open_pull(
        &self,
        selector: &str,
        options: PullOptions,
        proposer: Option<Proposer>,
    ) -> Result<OpenOutcome> {
        // Cleanup holds the workspace's gate, so it cannot remove the
        // workspace between opening the PR and linking it.
        let (workspace, _gate) = self.lock_pr_gate(selector).await?;
        self.verify_worktree(&workspace).await?;
        let head = super::pr::current_head(&workspace).await?;
        anyhow::ensure!(
            workspace.state.accepts_links(),
            "workspace cannot accept a PR in its current state"
        );
        let settings = self.workspace_settings(&workspace).await?;
        let required = &settings.pr.required_labels;
        let (forge, account) = self.workspace_forge_account(&workspace).await?;
        let head_repository = head_repository(&workspace.path, &workspace.branch, &forge).await?;
        let existing = forge
            .open_pull_for(&account, &workspace.branch, head_repository.as_deref())
            .await?;
        let (item, body, unfinished) = match existing {
            Some(number) => {
                let item = forge.read(&account, ItemKind::Pr, number).await?.item;
                (item, None, None)
            }
            None => {
                conventions::require(&forge, &account, ItemKind::Pr, required, &options.labels)
                    .await?;
                let pull = NewPull {
                    head_repository,
                    ..new_pull(&workspace, &forge, &account, options, settings.pr.draft).await?
                };
                if let Some(proposer) =
                    proposer.filter(|_| settings.approvals.pr_open == Opening::Propose)
                {
                    forge.check_uploads(&account, &pull.attachments).await?;
                    let proposed = self.propose_pull(&workspace, pull, head, proposer).await?;
                    return Ok(OpenOutcome::Proposed(proposed));
                }
                let created = forge.create_pull(&account, &pull).await?;
                (created.item, Some(pull.body), created.unfinished)
            }
        };
        let created = body.is_some();
        anyhow::ensure!(
            item.pr
                .as_ref()
                .is_some_and(|pr| pr.head == workspace.branch),
            "{} does not have the workspace branch as its head",
            item.url
        );
        let (linked, warning) = if created {
            self.keep_created(&workspace, ItemKind::Pr, &item, true)
                .await?
        } else {
            self.set_pr_gated(&workspace.id, watch(&item), super::pr::WatchCheck::Known)
                .await
                .with_context(|| format!("{} is open, but linking it failed", item.url))?;
            (true, None)
        };
        let checked = conventions::warnings(
            &forge,
            &account,
            &workspace.path,
            &item,
            body.as_deref(),
            required,
        )
        .await;
        Ok(OpenOutcome::Opened(Opened {
            item,
            created,
            linked,
            warnings: warning
                .into_iter()
                .chain(unfinished)
                .chain(checked)
                .collect(),
        }))
    }

    /// Record an item the workspace just created as opened and, with `link`,
    /// assign it. A removal that starts after the caller's check leaves it
    /// unrecorded or unlinked with a warning, so nothing fails once the item
    /// exists and a retry cannot create a duplicate. Returns whether it was
    /// linked, and the warning.
    pub(super) async fn keep_created(
        &self,
        workspace: &Workspace,
        kind: ItemKind,
        item: &Item,
        link: bool,
    ) -> Result<(bool, Option<String>)> {
        let (id, url, title) = (workspace.id.clone(), item.url.clone(), item.title.clone());
        let recorded = self
            .store
            .run(move |db| record_opened(db, &id, kind, &url, &title))
            .await
            .with_context(|| format!("{} was opened, but recording it failed", item.url))?;
        if !recorded || !link {
            return Ok((false, (!recorded).then(|| removal_warning(&item.url))));
        }
        let linked = match kind {
            ItemKind::Issue => {
                let title = Some(item.title.clone());
                self.set_issue_gated(&workspace.id, &item.url, title).await
            }
            ItemKind::Pr => {
                let check = super::pr::WatchCheck::Known;
                self.set_pr_gated(&workspace.id, watch(item), check).await
            }
        };
        match linked {
            Ok(()) => Ok((true, None)),
            Err(_) if !self.accepts_links(&workspace.id).await? => {
                Ok((false, Some(removal_warning(&item.url))))
            }
            Err(error) => {
                Err(error.context(format!("{} was opened, but linking it failed", item.url)))
            }
        }
    }

    /// Whether the workspace still exists and takes link changes.
    async fn accepts_links(&self, id: &str) -> Result<bool> {
        let id = id.to_owned();
        self.store
            .run(move |db| super::accepts_links(db, &id))
            .await
    }
}

fn watch(item: &Item) -> super::pr::Action {
    super::pr::Action::Watch {
        url: item.url.clone(),
    }
}

/// Record an item the workspace created as opened, with the event of its
/// creation, in one transaction, returning whether it was recorded: a
/// removal that started after the caller's check takes no link changes, and
/// the item exists either way.
fn record_opened(
    db: &mut rusqlite::Connection,
    id: &str,
    kind: ItemKind,
    url: &str,
    title: &str,
) -> Result<bool> {
    let tx = db.transaction()?;
    events::record_item(&tx, id, kind, url, "open")?;
    let recorded = super::accepts_links(&tx, id)?;
    if recorded {
        super::record(&tx, id, Relation::Opened, kind, url, Some(title))?;
    }
    tx.commit()?;
    Ok(recorded)
}

/// The item a mark names as the forge reports it; none for a workspace mark
/// or a failed read.
async fn marked_item(forge: &ForgeRepo, account: &Account, mark: &ReviewMark) -> Option<Item> {
    let (kind, url) = (mark.kind?, mark.url.as_deref()?);
    let (number, _) = match kind {
        ItemKind::Issue => forge.issue(url),
        ItemKind::Pr => forge.pull(url),
    }
    .ok()?;
    Some(forge.read(account, kind, number).await.ok()?.item)
}

/// The labels `[pr]` or `[issue]` requires of an item of `kind`.
pub(super) fn required_labels(settings: &Effective, kind: ItemKind) -> &[String] {
    match kind {
        ItemKind::Issue => &settings.issue.required_labels,
        ItemKind::Pr => &settings.pr.required_labels,
    }
}

/// The warning for an item created after removal started.
fn removal_warning(url: &str) -> String {
    format!("{url} was opened, but removal started first, so the workspace does not link it")
}

/// Fill what the caller left unset: the title from the linked issue, then
/// the last commit; a closing reference to the linked issue; the base
/// workspace's branch, then the default branch; draft state from
/// `default_draft`.
async fn new_pull(
    workspace: &Workspace,
    forge: &ForgeRepo,
    account: &Account,
    options: PullOptions,
    default_draft: bool,
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
        (None, None, None) => crate::git::latest_subject(&workspace.path)
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
        head_repository: None,
        base,
        draft: options.draft.unwrap_or(default_draft),
        labels: options.labels,
        reviewers: options.reviewers,
        attachments: options.attachments,
    })
}

/// Use a supplied description verbatim; otherwise close the linked issue.
fn description(body: Option<String>, issue: Option<u64>) -> String {
    let Some(number) = issue else {
        return body.unwrap_or_default();
    };
    let closing = format!("Closes #{number}");
    body.unwrap_or(closing)
}

/// `owner/repository` of the fork the branch's push remote names, or `None`
/// when it pushes to the PR's own repository. A remote is named by where it
/// fetches from, as `origin` is, whatever its push URL.
pub(super) async fn head_repository(
    worktree: &std::path::Path,
    branch: &str,
    forge: &ForgeRepo,
) -> Result<Option<String>> {
    let remote = crate::git::push_remote(worktree, branch).await?;
    let url = crate::git::remote_url(worktree, &remote).await?;
    let pushed = ForgeRepo::parse(url.trim())
        .with_context(|| format!("the branch's push remote {remote} is not a forge repository"))?;
    if pushed == *forge {
        return Ok(None);
    }
    anyhow::ensure!(
        pushed.host == forge.host,
        "the branch's push remote {remote} is on {}, so a PR on {} cannot have it as its head",
        pushed.host,
        forge.host
    );
    Ok(Some(pushed.path))
}

pub(super) fn linked(workspace: &Workspace, kind: ItemKind) -> Result<String> {
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
    fn linked_issue_closes_only_without_a_description() {
        assert_eq!(description(None, None), "");
        assert_eq!(description(Some("Fix".into()), None), "Fix");
        assert_eq!(description(None, Some(7)), "Closes #7");
        assert_eq!(description(Some("Fix\n".into()), Some(7)), "Fix\n");
        assert_eq!(description(Some("Refs #7".into()), Some(7)), "Refs #7");
        assert_eq!(description(Some("".into()), Some(7)), "");
    }

    #[tokio::test]
    async fn a_branch_pushed_to_a_fork_on_the_same_forge_names_the_fork_as_its_head() {
        use crate::test_support::{git, repository};
        let root = tempfile::tempdir().unwrap();
        let path = repository(root.path(), "repo");
        let forge = ForgeRepo::parse("https://github.com/team/repo").unwrap();
        let head = || head_repository(&path, "topic", &forge);
        git(
            &path,
            &["remote", "add", "origin", "git@github.com:team/repo.git"],
        );
        assert_eq!(head().await.unwrap(), None);
        git(
            &path,
            &["remote", "add", "fork", "git@github.com:someone/repo.git"],
        );
        git(&path, &["config", "remote.pushDefault", "fork"]);
        assert_eq!(head().await.unwrap().as_deref(), Some("someone/repo"));
        git(
            &path,
            &["remote", "set-url", "--push", "fork", "/elsewhere/repo.git"],
        );
        // A push URL elsewhere does not change which repository the remote is.
        assert_eq!(head().await.unwrap().as_deref(), Some("someone/repo"));
        git(
            &path,
            &[
                "remote",
                "set-url",
                "fork",
                "https://gitlab.com/someone/repo",
            ],
        );
        let error = head().await.unwrap_err();
        assert!(error.to_string().contains("gitlab.com"), "{error:#}");
    }

    #[tokio::test]
    async fn a_workspace_being_removed_opens_nothing_and_keeps_late_items_unlinked() {
        use crate::{
            state::WorkspaceState,
            test_support::{manager, repository},
        };
        let (root, manager) = manager().await;
        let path = repository(root.path(), "repo");
        let repo = manager
            .register_repository(path.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "topic".into(), None, None, None)
            .await
            .unwrap();
        let item = |number: u64| Item {
            kind: ItemKind::Issue,
            number,
            url: format!("https://forge.example/team/repo/issues/{number}"),
            title: "Follow-up".into(),
            state: ItemState::Open,
            labels: Vec::new(),
            pr: None,
        };

        // A link failing for another reason still fails, after the record.
        let error = manager
            .keep_created(&workspace, ItemKind::Issue, &item(1), true)
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("linking it failed"),
            "{error:#}"
        );
        let links = manager.workspace(&workspace.id).await.unwrap().links;
        assert_eq!(links.opened.len(), 1);

        manager
            .reserve_lifecycle(&workspace.id, WorkspaceState::Removing)
            .await
            .unwrap();
        // Refused before the forge is consulted: the repository has no origin.
        let issue = NewIssue {
            title: "Follow-up".into(),
            body: String::new(),
            labels: Vec::new(),
            attachments: Vec::new(),
        };
        let error = manager
            .open_issue(&workspace.id, issue, None, false, None)
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("cannot accept an issue"),
            "{error:#}"
        );
        let error = manager
            .open_pull(&workspace.id, PullOptions::default(), None)
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("cannot accept a PR"),
            "{error:#}"
        );

        // An item created after the check but before removal reserved the
        // workspace keeps its creation event and comes back with a warning.
        let (linked, warning) = manager
            .keep_created(&workspace, ItemKind::Issue, &item(2), true)
            .await
            .unwrap();
        assert!(!linked);
        assert!(warning.unwrap().contains("removal started"));
        let links = manager.workspace(&workspace.id).await.unwrap().links;
        assert_eq!(links.opened.len(), 1);
        let created = manager
            .workspace_events(None, 100)
            .await
            .unwrap()
            .into_iter()
            .filter(|item| {
                matches!(item,
                crate::daemon::events::EventItem::Event(event) if event.details.item.is_some())
            })
            .count();
        assert_eq!(created, 2);
    }
}
