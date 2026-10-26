use super::*;

#[test]
fn only_failures_a_login_may_fix_carry_the_login_hint() {
    let hinted = |error: anyhow::Error| {
        let failed: Result<()> = Err(error.context("GET pulls/1"));
        format!("{:#}", failed.hint_login(&ForgeKind::Forgejo).unwrap_err())
    };
    let refused = hinted(Transient::Unsent("curl failed (exit status: 7)".into()).into());
    assert_eq!(refused, "GET pulls/1: curl failed (exit status: 7)");
    let denied = hinted(anyhow::anyhow!("HTTP 404"));
    assert_eq!(
        denied,
        format!("{}: GET pulls/1: HTTP 404", ForgeKind::Forgejo.login_hint())
    );
}

#[test]
fn well_known_and_gitlab_named_hosts_are_recognized() {
    for (host, kind) in [
        ("github.com", ForgeKind::GitHub),
        ("gitlab.com", ForgeKind::GitLab),
        ("bitbucket.org", ForgeKind::Bitbucket),
        ("gitlab.corp.example", ForgeKind::GitLab),
        ("git.corp.example", ForgeKind::Forgejo),
        ("mygitlab.example", ForgeKind::Forgejo),
        ("codeberg.org", ForgeKind::Forgejo),
    ] {
        assert_eq!(ForgeKind::detect(host, None), kind, "{host}");
    }
}
