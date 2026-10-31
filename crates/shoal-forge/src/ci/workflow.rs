//! Workflows named by their web URLs: a workflow's page, or its file on a
//! branch, which a run then starts on by default.
use anyhow::{Context, Result, ensure};

use crate::{
    ForgeRepo, IssueInput, account::Account, backends::Backend, ci::run::workflow_segment,
    hosts::HostTypes, items::link::decode_branch, transport::api::Request,
};

const WORKFLOW_URL: &str = "expected a workflow URL ending in /actions/workflows/<file> or \
    /actions?workflow=<file>, or a workflow file's URL";

/// What precedes the branch in a file's web URL, and the directories that
/// forge reads workflow files from.
const FILE_ROUTES: &[(&str, &[&str])] = &[
    ("/blob/", &["/.github/workflows/"]),
    (
        "/src/branch/",
        &[
            "/.forgejo/workflows/",
            "/.gitea/workflows/",
            "/.github/workflows/",
        ],
    ),
];

/// A workflow its web URL names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowLink {
    /// The workflow's file name.
    pub workflow: String,
    /// The branch a workflow file's URL names.
    pub branch: Option<String>,
}

/// Whether `input` names a workflow by its web URL rather than by its file
/// name or ID.
pub fn is_workflow_url(input: &str) -> bool {
    IssueInput::parse(input) == IssueInput::Url
}

impl WorkflowLink {
    /// The workflow `url` names, with its repository's URL.
    pub(crate) fn parse(url: &str) -> Result<(&str, Self)> {
        ensure!(is_workflow_url(url), WORKFLOW_URL);
        let url = url.split('#').next().unwrap_or_default();
        let (path, query) = url.split_once('?').unwrap_or((url, ""));
        let path = path.trim_end_matches('/');
        let (repository, workflow, branch) = page(path, query)
            .or_else(|| file(path))
            .context(WORKFLOW_URL)?;
        let branch = branch.map(decode_branch).transpose()?;
        ensure!(
            branch.as_ref().is_none_or(|branch| !branch.is_empty()),
            "the workflow file's URL names no branch"
        );
        Ok((
            repository,
            Self {
                workflow: workflow_segment(workflow)?.into(),
                branch,
            },
        ))
    }
}

/// The repository URL and workflow of a workflow's page: GitHub's
/// `…/actions/workflows/<file>`, or Forgejo's `…/actions?workflow=<file>`.
fn page<'a>(path: &'a str, query: &'a str) -> Option<(&'a str, &'a str, Option<&'a str>)> {
    if let Some((repository, workflow)) = path.rsplit_once("/actions/workflows/") {
        return (!workflow.contains('/')).then_some((repository, workflow, None));
    }
    let repository = path.strip_suffix("/actions")?;
    let workflow = query
        .split('&')
        .find_map(|pair| pair.strip_prefix("workflow="))?;
    Some((repository, workflow, None))
}

/// The repository URL, workflow and branch of a workflow file's URL.
fn file(path: &str) -> Option<(&str, &str, Option<&str>)> {
    FILE_ROUTES.iter().find_map(|(marker, directories)| {
        let (repository, rest) = path.split_once(marker)?;
        directories.iter().find_map(|directory| {
            let (branch, workflow) = rest.split_once(directory)?;
            (!workflow.contains('/')).then_some((repository, workflow, Some(branch)))
        })
    })
}

impl ForgeRepo {
    /// The repository a workflow's web URL belongs to, on the forge `hosts`
    /// says its host serves, and the workflow it names.
    pub async fn resolve_workflow_url(
        url: &str,
        hosts: &impl HostTypes,
    ) -> Result<(Self, WorkflowLink)> {
        let (repository, link) = WorkflowLink::parse(url)?;
        Ok((Self::resolve(repository, hosts).await?, link))
    }

    /// The repository's default branch.
    pub async fn default_branch(&self, account: &Account) -> Result<String> {
        let endpoint = self.repo_endpoint("");
        let read = Request::get(endpoint.trim_end_matches('/').into());
        let value = self.send(account, &read).await?;
        let branch = self.kind.default_branch(&value);
        branch
            .map(Into::into)
            .with_context(|| format!("{} reports no default branch", self.repository_url()))
    }
}

#[cfg(test)]
mod tests;
