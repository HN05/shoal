use serde_json::json;

use super::*;
use crate::transport::api::HttpMethod;

fn run(workflow: Option<&str>) -> NewRun {
    NewRun {
        workflow: workflow.map(Into::into),
        branch: "topic".into(),
        inputs: BTreeMap::from([("level".into(), "full".into())]),
    }
}

/// The method, endpoint and body of the request that starts `run`.
fn request(remote: &str, run: &NewRun) -> Result<(HttpMethod, String, serde_json::Value)> {
    let repo = ForgeRepo::parse(remote)?;
    let request = repo.kind.start_run(&repo, run)?;
    Ok((
        request.method,
        request.endpoint,
        request.body.unwrap_or_default(),
    ))
}

#[test]
fn each_forge_starts_a_run_its_own_way() -> Result<()> {
    assert_eq!(
        request("https://github.com/team/repo", &run(Some("ci.yml")))?,
        (
            HttpMethod::Post,
            "repos/team/repo/actions/workflows/ci.yml/dispatches".into(),
            json!({"ref": "topic", "inputs": {"level": "full"}})
        )
    );
    assert_eq!(
        request("https://codeberg.org/team/repo", &run(Some("ci.yml")))?,
        (
            HttpMethod::Post,
            "repos/team/repo/actions/workflows/ci.yml/dispatches".into(),
            json!({"ref": "topic", "inputs": {"level": "full"}, "return_run_info": true})
        )
    );
    assert_eq!(
        request("https://gitlab.com/group/sub/repo", &run(None))?,
        (
            HttpMethod::Post,
            "projects/group%2Fsub%2Frepo/pipeline".into(),
            json!({"ref": "topic", "inputs": {"level": "full"}})
        )
    );
    assert_eq!(
        request("https://bitbucket.org/team/repo", &run(Some("nightly")))?,
        (
            HttpMethod::Post,
            "repositories/team/repo/pipelines".into(),
            json!({
                "target": {
                    "type": "pipeline_ref_target",
                    "ref_type": "branch",
                    "ref_name": "topic",
                    "selector": {"type": "custom", "pattern": "nightly"}
                },
                "variables": [{"key": "level", "value": "full"}]
            })
        )
    );
    // Without a custom pipeline, Bitbucket runs the branch's own.
    let (_, _, body) = request("https://bitbucket.org/team/repo", &run(None))?;
    assert!(body["target"].get("selector").is_none(), "{body}");
    Ok(())
}

#[test]
fn a_run_names_a_workflow_only_where_the_forge_takes_one() -> Result<()> {
    let error = request("https://github.com/team/repo", &run(None)).unwrap_err();
    assert_eq!(
        error.to_string(),
        "GitHub starts a named workflow; name its file, such as ci.yml"
    );
    let error = request("https://github.com/team/repo", &run(Some("../merge"))).unwrap_err();
    assert!(error.to_string().contains("not a workflow file"), "{error}");
    let error = request("https://gitlab.com/team/repo", &run(Some("ci.yml"))).unwrap_err();
    assert!(
        error.to_string().contains("leave out the workflow"),
        "{error}"
    );
    Ok(())
}

#[test]
fn started_runs_link_the_run_where_the_forge_reports_it() -> Result<()> {
    let url = |remote: &str, response| -> Result<Option<String>> {
        let repo = ForgeRepo::parse(remote)?;
        Ok(repo.kind.started_run_url(&repo, &response))
    };
    assert_eq!(url("https://github.com/team/repo", json!(null))?, None);
    assert_eq!(
        url(
            "https://codeberg.org/team/repo",
            json!({"id": 9331, "run_number": 2765})
        )?,
        Some("https://codeberg.org/team/repo/actions/runs/2765".into())
    );
    // Gitea and older Forgejo report no run.
    assert_eq!(url("https://codeberg.org/team/repo", json!(null))?, None);
    let web_url = "https://gitlab.com/team/repo/-/pipelines/12";
    assert_eq!(
        url("https://gitlab.com/team/repo", json!({"web_url": web_url}))?,
        Some(web_url.into())
    );
    assert_eq!(
        url(
            "https://bitbucket.org/team/repo",
            json!({"build_number": 7})
        )?,
        Some("https://bitbucket.org/team/repo/pipelines/results/7".into())
    );
    Ok(())
}

#[test]
fn runs_need_a_branch_and_named_inputs() {
    let mut blank = run(Some("ci.yml"));
    blank.branch = " ".into();
    assert!(blank.validate().is_err());
    let mut unnamed = run(Some("ci.yml"));
    unnamed.inputs.insert(String::new(), "x".into());
    assert!(unnamed.validate().is_err());
    assert!(run(Some("ci.yml")).validate().is_ok());
}
