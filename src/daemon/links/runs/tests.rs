use super::*;

fn started(url: &str) -> StartedRun {
    StartedRun {
        workflow: Some("deploy".into()),
        branch: "topic".into(),
        url: Some(url.into()),
        linked: false,
    }
}

#[test]
fn a_started_run_is_linked_only_where_shoal_reads_runs() -> Result<()> {
    let forgejo = ForgeRepo::parse("https://codeberg.org/team/repo")?;
    let link = started_link(
        &forgejo,
        &started("https://codeberg.org/team/repo/actions/runs/2765"),
    );
    assert_eq!(
        link.map(|item| item.url).as_deref(),
        Some("https://codeberg.org/team/repo/actions/runs/2765")
    );
    // Bitbucket reports a pipeline's URL but Shoal reads no pipelines, so
    // the started pipeline goes unlinked rather than failing the start.
    let bitbucket = ForgeRepo::parse("https://bitbucket.org/team/repo")?;
    let pipeline = started("https://bitbucket.org/team/repo/pipelines/results/7");
    assert_eq!(started_link(&bitbucket, &pipeline), None);
    Ok(())
}
