use serde_json::json;

use super::*;
use crate::backends::ForgeKind;

fn parsed(url: &str) -> Result<(&str, String, Option<String>)> {
    let (repository, link) = WorkflowLink::parse(url)?;
    Ok((repository, link.workflow, link.branch))
}

#[test]
fn workflow_pages_name_the_workflow_without_a_branch() -> Result<()> {
    for url in [
        "https://github.com/team/repo/actions/workflows/ci.yml",
        "https://github.com/team/repo/actions/workflows/ci.yml/?query=branch%3Amain#runs",
        "https://codeberg.org/team/repo/actions?workflow=ci.yml",
        "https://codeberg.org/team/repo/actions/?actor=0&workflow=ci.yml&status=0",
    ] {
        let (repository, workflow, branch) = parsed(url)?;
        assert!(repository.ends_with("/team/repo"), "{url}: {repository}");
        assert_eq!((workflow.as_str(), branch), ("ci.yml", None), "{url}");
    }
    Ok(())
}

#[test]
fn workflow_file_urls_name_the_workflow_on_their_branch() -> Result<()> {
    for (url, branch) in [
        (
            "https://github.com/team/repo/blob/main/.github/workflows/ci.yml",
            "main",
        ),
        (
            "https://github.com/team/repo/blob/feature/x/.github/workflows/ci.yml?plain=1",
            "feature/x",
        ),
        (
            "https://codeberg.org/team/repo/src/branch/release%231/.forgejo/workflows/ci.yml",
            "release#1",
        ),
        (
            "https://gitea.example/team/repo/src/branch/main/.gitea/workflows/ci.yml",
            "main",
        ),
        (
            "https://codeberg.org/team/repo/src/branch/main/.github/workflows/ci.yml",
            "main",
        ),
    ] {
        let (repository, workflow, found) = parsed(url)?;
        assert!(repository.ends_with("/team/repo"), "{url}: {repository}");
        assert_eq!(
            (workflow.as_str(), found.as_deref()),
            ("ci.yml", Some(branch)),
            "{url}"
        );
    }
    Ok(())
}

#[test]
fn other_links_name_no_workflow() {
    for url in [
        "ci.yml",
        "https://github.com/team/repo",
        "https://github.com/team/repo/actions",
        "https://github.com/team/repo/actions/runs/12",
        "https://github.com/team/repo/actions/workflows/ci.yml/badge.svg",
        "https://github.com/team/repo/blob/main/.github/workflows/sub/ci.yml",
        "https://github.com/team/repo/blob/main/ci.yml",
        // GitHub runs only the workflows under .github.
        "https://github.com/team/repo/blob/main/.forgejo/workflows/ci.yml",
        "https://codeberg.org/team/repo/actions?workflow=..",
    ] {
        assert!(WorkflowLink::parse(url).is_err(), "{url}");
    }
    let error = WorkflowLink::parse("https://github.com/team/repo/blob//.github/workflows/ci.yml")
        .unwrap_err();
    assert_eq!(error.to_string(), "the workflow file's URL names no branch");
}

#[test]
fn a_run_refuses_a_link_that_names_no_workflow_before_any_lookup() {
    assert!(validate_run_link("https://github.com/team/repo/actions/workflows/ci.yml").is_ok());
    let error = validate_run_link("https://github.com/team/repo/pull/8").unwrap_err();
    assert!(
        error.to_string().starts_with("expected a workflow URL"),
        "{error}"
    );
}

fn validate_run_link(url: &str) -> Result<()> {
    crate::ci::run::validate_run(Some(url), None)
}

#[test]
fn each_forge_reports_its_default_branch_its_own_way() {
    let repository = json!({"default_branch": "main", "mainbranch": {"name": "trunk"}});
    for (kind, branch) in [
        (ForgeKind::GitHub, "main"),
        (ForgeKind::Forgejo, "main"),
        (ForgeKind::GitLab, "main"),
        (ForgeKind::Bitbucket, "trunk"),
    ] {
        assert_eq!(kind.default_branch(&repository), Some(branch), "{kind}");
    }
}
