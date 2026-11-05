use super::*;

#[test]
fn a_refusal_names_the_capability() -> Result<()> {
    let forgejo = ForgeRepo::parse("https://codeberg.org/o/r.git")?;
    let error = forgejo.require(Capability::ResolveThreads).unwrap_err();
    let unsupported = error.downcast_ref::<Unsupported>();
    assert_eq!(
        unsupported.map(|refusal| refusal.capability),
        Some(Capability::ResolveThreads)
    );
    assert!(forgejo.require(Capability::CancelJobs).is_ok());
    Ok(())
}

#[tokio::test]
async fn forgejo_reruns_jobs_from_release_17_and_runs_them_from_release_15() -> Result<()> {
    use crate::{
        backends::ForgeKind,
        test_support::{Server, agent_on, repo_on},
    };
    for (version, reruns, runs) in [
        ("17.0.1+gitea-1.22.0", true, true),
        ("15.0.0+gitea-1.22.0", false, true),
        ("14.0.0+gitea-1.22.0", false, false),
        ("1.25.0", false, false),
    ] {
        let body = format!(r#"{{"version":"{version}"}}"#);
        let server = Server::start(move |_| (200, Vec::new(), body.clone().into()));
        let forgejo = repo_on(&server, ForgeKind::Forgejo, "team/repo");
        let account = agent_on(&server, "agent");
        let listed = forgejo.server_capabilities(&account).await?;
        let supported = |capability| {
            listed
                .capabilities
                .iter()
                .find(|entry| entry.capability == capability)
                .map(|entry| entry.supported)
        };
        assert_eq!(supported(Capability::RerunJobs), Some(reruns), "{version}");
        assert_eq!(supported(Capability::RunJobs), Some(runs), "{version}");
        let required = forgejo
            .require_on_server(&account, Capability::RerunJobs)
            .await;
        assert_eq!(required.is_ok(), reruns, "{version}");
        // Only the version is read, once for each question.
        let lines: Vec<_> = server.received().into_iter().map(|r| r.line).collect();
        assert_eq!(lines, ["GET /api/v1/version"; 3]);
    }
    Ok(())
}
