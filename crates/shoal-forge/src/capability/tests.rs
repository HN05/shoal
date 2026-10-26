use super::*;

#[test]
fn a_refusal_names_the_forge_the_capability_and_the_hint() -> Result<()> {
    let forgejo = ForgeRepo::parse("https://codeberg.org/o/r.git")?;
    let error = forgejo.require(Capability::ResolveThreads).unwrap_err();
    let unsupported = error.downcast_ref::<Unsupported>();
    assert_eq!(
        unsupported.map(|refusal| refusal.capability),
        Some(Capability::ResolveThreads)
    );
    assert_eq!(
        error.to_string(),
        "Shoal cannot resolve review threads on Forgejo or Gitea; reply in the thread instead"
    );
    assert!(forgejo.require(Capability::CancelJobs).is_ok());
    Ok(())
}

#[test]
fn capabilities_list_every_capability_in_order() -> Result<()> {
    let github = ForgeRepo::parse("https://github.com/o/r.git")?.capabilities();
    assert_eq!(github.forge, "GitHub");
    let listed: Vec<_> = github
        .capabilities
        .iter()
        .map(|entry| entry.capability)
        .collect();
    assert_eq!(listed, Capability::ALL);
    let bitbucket = ForgeRepo::parse("https://bitbucket.org/o/r.git")?.capabilities();
    let reopen = bitbucket
        .capabilities
        .iter()
        .find(|entry| entry.capability == Capability::ReopenPrs);
    assert_eq!(reopen.map(|entry| entry.supported), Some(false));
    Ok(())
}
