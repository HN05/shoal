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
