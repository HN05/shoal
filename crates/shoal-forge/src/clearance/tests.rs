use super::*;
use crate::{ci::JobState, numbers::RunId};

fn run(id: u64, state: JobState) -> Run {
    Run {
        id: RunId::new(id),
        url: format!("https://forge.example/o/r/actions/runs/{id}"),
        branch: Some("main".into()),
        workflow: Some("ci.yml".into()),
        commit: Some("0123456789abcdef".into()),
        state,
        waiting: false,
    }
}

#[test]
fn an_issue_clears_once_closed_and_a_pr_once_merged_or_closed() {
    assert_eq!(
        item_clearance(ItemKind::Issue, ItemState::Open),
        Clearance::Open
    );
    assert_eq!(
        item_clearance(ItemKind::Pr, ItemState::Open),
        Clearance::Open
    );
    let cleared = |kind, state| matches!(item_clearance(kind, state), Clearance::Cleared(_));
    assert!(cleared(ItemKind::Issue, ItemState::Closed));
    assert!(cleared(ItemKind::Pr, ItemState::Merged));
    assert!(cleared(ItemKind::Pr, ItemState::Closed));
}

#[test]
fn a_run_clears_once_a_later_run_on_the_default_branch_passes() {
    let failed = run(10, JobState::Failed);
    assert_eq!(later_run_clearance(&failed, "main", None), Clearance::Open);
    // An earlier pass says nothing of what broke the run.
    let earlier = run(9, JobState::Finished);
    assert_eq!(
        later_run_clearance(&failed, "main", Some(&earlier)),
        Clearance::Open
    );
    let still_failing = run(11, JobState::Failed);
    assert_eq!(
        later_run_clearance(&failed, "main", Some(&still_failing)),
        Clearance::Open
    );
    let fixed = run(12, JobState::Finished);
    assert_eq!(
        later_run_clearance(&failed, "main", Some(&fixed)),
        Clearance::Cleared(format!("run {} passed on main at 0123456789ab", fixed.url))
    );
}

#[tokio::test]
async fn blocking_runs_of_one_workflow_share_one_read_of_the_default_branch() -> Result<()> {
    use crate::{
        backends::ForgeKind,
        test_support::{Server, agent_on, repo_on},
    };
    use serde_json::json;
    let forgejo_run = |id: u64, number: u64, branch: &str, status: &str| {
        json!({"id": id, "index_in_repo": number, "prettyref": branch,
            "workflow_id": "ci.yml", "commit_sha": "0123456789abcdef", "status": status})
    };
    let server = Server::start(move |request| {
        let body = if request.line.contains("workflow_id=ci.yml") {
            json!({"workflow_runs": [forgejo_run(912, 12, "main", "success")]})
        } else if request.line.contains("actions/runs") {
            // Both blocking runs failed on a topic branch before the fix.
            let number: u64 = if request.line.contains("11") { 11 } else { 10 };
            json!({"workflow_runs": [forgejo_run(900 + number, number, "topic", "failure")]})
        } else {
            json!({"default_branch": "main"})
        };
        (200, Vec::new(), body.to_string().into())
    });
    let forgejo = repo_on(&server, ForgeKind::Forgejo, "team/repo");
    let account = agent_on(&server, "agent");
    let mut newest = NewestRuns::default();
    for number in [10, 11] {
        let url = format!("{}/actions/runs/{number}", forgejo.repository_url());
        let found = forgejo.run_clearance(&account, &url, &mut newest).await?;
        let fixed = format!("{}/actions/runs/12", forgejo.repository_url());
        assert_eq!(
            found,
            Clearance::Cleared(format!("run {fixed} passed on main at 0123456789ab"))
        );
    }
    let listings = (server.received().iter())
        .filter(|request| request.line.contains("workflow_id=ci.yml"))
        .count();
    assert_eq!(listings, 1);
    Ok(())
}
