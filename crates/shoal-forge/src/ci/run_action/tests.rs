use serde_json::json;

use super::*;

fn run(id: u64) -> Run {
    Run {
        id,
        url: String::new(),
        branch: None,
        workflow: None,
        state: JobState::Finished,
    }
}

#[test]
fn run_urls_locate_runs_only_in_the_repository() -> Result<()> {
    let locate = |remote: &str, target: &str| -> Result<Option<u64>> {
        let repo = ForgeRepo::parse(remote)?;
        Ok(repo.kind.locate_run(&repo, target))
    };
    let github = "https://github.com/team/repo";
    assert_eq!(
        locate(github, &format!("{github}/actions/runs/12"))?,
        Some(12)
    );
    assert_eq!(
        locate(github, &format!("{github}/actions/runs/12/job/34"))?,
        Some(12)
    );
    assert_eq!(
        locate(github, "https://github.com/other/repo/actions/runs/12")?,
        None
    );
    let gitlab = "https://gitlab.com/group/sub/repo";
    assert_eq!(
        locate(gitlab, &format!("{gitlab}/-/pipelines/56"))?,
        Some(56)
    );
    assert_eq!(locate(gitlab, &format!("{gitlab}/-/jobs/56"))?, None);
    let forgejo = "https://forge.example/team/repo";
    assert_eq!(
        locate(forgejo, &format!("{forgejo}/actions/runs/2765"))?,
        Some(2765)
    );
    assert_eq!(
        locate(forgejo, "/team/repo/actions/runs/2765/jobs/1")?,
        Some(2765)
    );
    assert_eq!(
        locate(forgejo, "https://forge.example/team/repo/issues/3")?,
        None
    );
    Ok(())
}

#[test]
fn each_forge_acts_on_a_run_its_own_way() -> Result<()> {
    let endpoint = |remote: &str, action, failed| -> Result<String> {
        ForgeRepo::parse(remote)?
            .kind
            .run_endpoint(&run(9), action, failed)
    };
    let github = "https://github.com/team/repo";
    assert_eq!(
        endpoint(github, CiAction::Rerun, false)?,
        "actions/runs/9/rerun"
    );
    assert_eq!(
        endpoint(github, CiAction::Rerun, true)?,
        "actions/runs/9/rerun-failed-jobs"
    );
    assert_eq!(
        endpoint(github, CiAction::Cancel, false)?,
        "actions/runs/9/cancel"
    );
    // GitLab retries a pipeline's failed jobs either way.
    let gitlab = "https://gitlab.com/team/repo";
    assert_eq!(
        endpoint(gitlab, CiAction::Rerun, false)?,
        "pipelines/9/retry"
    );
    assert_eq!(
        endpoint(gitlab, CiAction::Cancel, false)?,
        "pipelines/9/cancel"
    );
    let forgejo = "https://codeberg.org/team/repo";
    assert_eq!(
        endpoint(forgejo, CiAction::Cancel, false)?,
        "actions/runs/9/cancel"
    );
    Ok(())
}

#[test]
fn runs_read_their_branch_workflow_and_state_from_the_forge() -> Result<()> {
    let read = |remote: &str, value| -> Result<Run> {
        let repo = ForgeRepo::parse(remote)?;
        repo.kind.run(&repo, 12, &value)
    };
    assert_eq!(
        read(
            "https://github.com/team/repo",
            json!({"id": 12, "html_url": "https://github.com/team/repo/actions/runs/12",
                    "head_branch": "topic", "path": ".github/workflows/ci.yml",
                    "status": "completed", "conclusion": "failure"})
        )?,
        Run {
            id: 12,
            url: "https://github.com/team/repo/actions/runs/12".into(),
            branch: Some("topic".into()),
            workflow: Some("ci.yml".into()),
            state: JobState::Failed,
        }
    );
    // A tag pipeline ran on no branch.
    let tag = read(
        "https://gitlab.com/team/repo",
        json!({"id": 12, "web_url": "https://gitlab.com/team/repo/-/pipelines/12",
                "ref": "v1", "tag": true, "status": "running"}),
    )?;
    assert_eq!((tag.branch, tag.state), (None, JobState::Running));
    let forgejo = read(
        "https://codeberg.org/team/repo",
        json!({"workflow_runs": [{"id": 9331, "index_in_repo": 12, "prettyref": "topic",
                "workflow_id": "deps.yml", "status": "success"}]}),
    )?;
    assert_eq!(
        forgejo,
        Run {
            id: 9331,
            url: "https://codeberg.org/team/repo/actions/runs/12".into(),
            branch: Some("topic".into()),
            workflow: Some("deps.yml".into()),
            state: JobState::Finished,
        }
    );
    let error = read(
        "https://codeberg.org/team/repo",
        json!({"workflow_runs": []}),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("did not identify one run"),
        "{error}"
    );
    Ok(())
}

#[test]
fn forges_without_run_actions_refuse_them_before_any_lookup() -> Result<()> {
    let forgejo = ForgeRepo::parse("https://codeberg.org/team/repo")?;
    let error = forgejo
        .require(Capability::ci(CiAction::Rerun))
        .unwrap_err();
    assert!(
        error.to_string().contains("cannot rerun CI jobs"),
        "{error}"
    );
    assert!(validate_run_reference("12").is_ok());
    assert!(validate_run_reference("ci.yml").is_err());
    assert!(validate_run_reference("0").is_err());
    Ok(())
}
