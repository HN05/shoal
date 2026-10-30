use super::*;
use crate::capability::Unsupported;

#[tokio::test]
async fn forges_without_a_revert_refuse_before_reading_the_pr() {
    let account = Account::user(&Default::default());
    let revert = NewRevert {
        title: "Revert \"Fix\"".into(),
        ..NewRevert::default()
    };
    for remote in [
        "https://forge.example/team/repo",
        "git@gitlab.com:group/repo.git",
        "git@bitbucket.org:team/repo.git",
    ] {
        let forge = ForgeRepo::parse(remote).unwrap();
        let error = forge.revert_pull(&account, 7, &revert).await.unwrap_err();
        let unsupported = error.downcast_ref::<Unsupported>().unwrap();
        assert_eq!(unsupported.capability, Capability::RevertPrs);
        assert!(error.to_string().contains("git revert"), "{error}");
    }
}
