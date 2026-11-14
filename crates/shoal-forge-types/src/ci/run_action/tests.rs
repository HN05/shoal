use super::*;
use crate::backends::Routes;

#[test]
fn run_urls_locate_runs_only_in_the_repository() -> Result<()> {
    let locate = |remote: &str, target: &str| -> Result<Option<u64>> {
        let repo = ForgeRepo::parse(remote)?;
        Ok(repo.kind.locate_run(&repo, target).map(RunNumber::get))
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
fn run_references_are_positive_numbers() {
    let repo = ForgeRepo::parse("https://github.com/team/repo").unwrap();
    assert!(repo.run("12").is_ok());
    assert!(repo.run("ci.yml").is_err());
    assert!(repo.run("0").is_err());
}
