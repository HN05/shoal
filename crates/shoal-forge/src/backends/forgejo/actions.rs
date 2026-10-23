//! Forgejo Actions: the runs that commit statuses link, and the logs of
//! failed jobs.
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use tokio::process::Command;

use super::tea;
use crate::{
    ForgeRepo,
    account::{Account, ForgejoLogin},
    ci::checks::{CheckLog, MAX_LOG_BYTES, NoJobLog},
    items::link::ItemKind,
    transport::{
        api::{Request, TIMEOUT, authorization, curl_body},
        programs,
    },
};

/// The log of the failed check `name` on the current head of PR `url`.
pub(crate) async fn failed_check_log(
    repo: &ForgeRepo,
    account: &Account,
    url: &str,
    name: &str,
) -> Result<CheckLog> {
    let (number, _) = repo.pull(url)?;
    let pull = repo
        .send(
            account,
            &Request::get(repo.item_endpoint(ItemKind::Pr, number)),
        )
        .await?;
    let head = pull["head"]["sha"]
        .as_str()
        .context("PR has no head commit")?;
    let statuses = repo
        .pages(
            account,
            &repo.repo_endpoint(&format!("commits/{head}/statuses")),
        )
        .await?;
    // Forgejo lists newest statuses first, including earlier attempts.
    let status = statuses
        .iter()
        .find(|status| status["context"].as_str() == Some(name))
        .context("check was not found on the PR's current head")?;
    ensure!(
        matches!(status["status"].as_str(), Some("failure" | "error")),
        "check has not failed"
    );
    let no_log = || NoJobLog { name: name.into() };
    let target = status["target_url"].as_str().ok_or_else(no_log)?;
    let (run_number, job_index) = job_location(repo, target).map_err(|_| no_log())?;
    let run_id = run_id_for(repo, account, run_number, head).await?;
    let jobs = repo
        .send(
            account,
            &Request::get(repo.repo_endpoint(&format!("actions/runs/{run_id}/jobs"))),
        )
        .await?;
    let job_id = job_id(&jobs, job_index, run_id)?;
    let endpoint = repo.repo_endpoint(&format!("actions/jobs/{job_id}/logs"));
    let log = job_log(repo, account, &endpoint).await?;
    Ok(CheckLog::from_text(url, name, &log).with_job(job_url(repo, target)))
}

/// Forgejo links a job by its path on the host.
fn job_url(repo: &ForgeRepo, target: &str) -> String {
    if target.starts_with('/') {
        format!("{}://{}{target}", repo.web_scheme, repo.host)
    } else {
        target.into()
    }
}

/// The API ID of the run numbered `number` in the repository, which must
/// have run on `head`.
pub(crate) async fn run_id_for(
    repo: &ForgeRepo,
    account: &Account,
    number: u64,
    head: &str,
) -> Result<u64> {
    let runs = repo
        .send(
            account,
            &Request::get(repo.repo_endpoint(&format!("actions/runs?run_number={number}"))),
        )
        .await?;
    run_id(&runs, number, head)
}

/// Web URLs use a repository run number and zero-based job index, neither
/// API ID. A host may link over HTTPS while its remote uses HTTP.
pub(crate) fn job_location(repo: &ForgeRepo, target: &str) -> Result<(u64, usize)> {
    let relative = ["https", "http"]
        .iter()
        .find_map(|scheme| target.strip_prefix(&format!("{scheme}://{}", repo.host)))
        .unwrap_or(target);
    let prefix = format!("/{}/actions/runs/", repo.path);
    let parts = relative
        .strip_prefix(&prefix)
        .context("check does not link to this repository's Actions job")?
        .split('/')
        .collect::<Vec<_>>();
    let [run, "jobs", job] = parts.as_slice() else {
        anyhow::bail!("invalid Actions job URL")
    };
    let run = run.parse::<u64>().context("invalid Actions run number")?;
    let job = job.parse::<usize>().context("invalid Actions job index")?;
    ensure!(run > 0, "invalid Actions run number");
    Ok((run, job))
}

/// Read plaintext logs without following redirects or exposing the login.
pub(crate) async fn job_log(repo: &ForgeRepo, account: &Account, endpoint: &str) -> Result<String> {
    let token = match account.forgejo_login(&repo.host).await? {
        ForgejoLogin::Tea(login) => {
            let request = Request::get(endpoint.to_owned());
            return tea::send(account, &login, &request, MAX_LOG_BYTES)
                .await
                .context("fetch Forgejo job log (maximum 8 MiB)");
        }
        ForgejoLogin::Token(token) => Some(token),
        ForgejoLogin::Anonymous => None,
    };
    let url = format!("{}://{}/api/v1/{endpoint}", repo.web_scheme, repo.host);
    let mut curl = Command::new(programs::CURL);
    curl.args([
        "-q",
        "-sS",
        "--max-time",
        "30",
        "--max-filesize",
        &MAX_LOG_BYTES.to_string(),
        "-H",
        "Accept: text/plain",
        "-w",
        "\n%{http_code}",
        "-K",
        "-",
        &url,
    ]);
    let output = shoal_subprocess::Run::new(curl)
        .input(token.map(authorization).unwrap_or_default())
        .timeout(TIMEOUT)
        // curl appends a newline and a three-digit HTTP status.
        .output_limit(MAX_LOG_BYTES + 4)
        .output()
        .await
        .context("fetch Forgejo job log (maximum 8 MiB)")?;
    Ok(curl_body(&output)?.to_owned())
}

fn run_id(runs: &Value, number: u64, head: &str) -> Result<u64> {
    let runs = runs["workflow_runs"]
        .as_array()
        .context("Actions runs are not a list")?;
    let [run] = runs.as_slice() else {
        anyhow::bail!("Actions run number did not identify one run")
    };
    ensure!(
        run["index_in_repo"] == number && run["commit_sha"] == head,
        "Actions run does not match the PR check's head"
    );
    run["id"]
        .as_u64()
        .filter(|id| *id > 0)
        .context("Actions run has no ID")
}

fn job_id(jobs: &Value, index: usize, run: u64) -> Result<u64> {
    let job = jobs
        .as_array()
        .and_then(|jobs| jobs.get(index))
        .context("Actions job index was not found")?;
    ensure!(
        job["run_id"] == run,
        "Actions job does not belong to the check's run"
    );
    ensure!(job["status"] == "failure", "Actions job has not failed");
    job["id"]
        .as_u64()
        .filter(|id| *id > 0)
        .context("Actions job has no ID")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn web_job_locations_are_repository_bound_and_use_indices() {
        let forge = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
        let http = ForgeRepo::parse("http://forge.example/team/repo").unwrap();
        for target in [
            "/team/repo/actions/runs/2765/jobs/1",
            "https://forge.example/team/repo/actions/runs/2765/jobs/1",
            "http://forge.example/team/repo/actions/runs/2765/jobs/1",
        ] {
            assert_eq!(job_location(&forge, target).unwrap(), (2765, 1));
            assert_eq!(job_location(&http, target).unwrap(), (2765, 1));
        }
        for target in [
            "https://evil.example/team/repo/actions/runs/2765/jobs/1",
            "/other/repo/actions/runs/2765/jobs/1",
            "/team/repo/actions/runs/0/jobs/1",
            "/team/repo/actions/runs/2765/jobs/1/logs",
        ] {
            assert!(job_location(&forge, target).is_err(), "{target}");
        }
        assert_eq!(
            job_url(&http, "/team/repo/actions/runs/2765/jobs/1"),
            "http://forge.example/team/repo/actions/runs/2765/jobs/1"
        );
        let runs =
            json!({"workflow_runs": [{"id": 9331, "index_in_repo": 2765, "commit_sha": "head"}]});
        assert_eq!(run_id(&runs, 2765, "head").unwrap(), 9331);
        assert!(run_id(&runs, 2765, "other").is_err());
        let jobs = json!([{"id": 18344, "run_id": 9331, "status": "success"}, {"id": 18345, "run_id": 9331, "status": "failure", "task_id": 9850}]);
        assert_eq!(job_id(&jobs, 1, 9331).unwrap(), 18345);
        assert!(job_id(&jobs, 0, 9331).is_err());
        assert!(job_id(&jobs, 1, 7).is_err());
    }
}
