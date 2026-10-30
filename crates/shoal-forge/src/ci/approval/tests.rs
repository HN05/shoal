use serde_json::json;

use super::*;

#[test]
fn each_forge_lists_the_runs_that_may_wait_its_own_way() -> Result<()> {
    let path = |remote: &str| -> Result<Option<String>> {
        Ok(ForgeRepo::parse(remote)?.kind.waiting_runs_path("abc", 2))
    };
    assert_eq!(
        path("https://github.com/team/repo")?.as_deref(),
        Some("actions/runs?head_sha=abc&status=action_required&per_page=50&page=2")
    );
    assert_eq!(
        path("https://codeberg.org/team/repo")?.as_deref(),
        Some("actions/runs?limit=50&page=2&head_sha=abc")
    );
    for remote in [
        "https://gitlab.com/team/repo",
        "https://bitbucket.org/team/repo",
    ] {
        assert_eq!(path(remote)?, None, "{remote}");
    }
    Ok(())
}

#[test]
fn a_run_waits_where_its_forge_says() -> Result<()> {
    let github = ForgeRepo::parse("https://github.com/team/repo")?;
    let run = |conclusion: &str| {
        json!({"id": 12, "html_url": "https://github.com/team/repo/actions/runs/12",
            "status": "completed", "conclusion": conclusion})
    };
    assert!(
        github
            .kind
            .run_item(&github, &run("action_required"))?
            .waiting
    );
    assert!(!github.kind.run_item(&github, &run("success"))?.waiting);
    let forgejo = ForgeRepo::parse("https://codeberg.org/team/repo")?;
    let run = |need_approval: bool| {
        json!({"id": 9331, "index_in_repo": 2765, "status": "waiting",
            "need_approval": need_approval})
    };
    assert!(forgejo.kind.run_item(&forgejo, &run(true))?.waiting);
    assert!(!forgejo.kind.run_item(&forgejo, &run(false))?.waiting);
    Ok(())
}
