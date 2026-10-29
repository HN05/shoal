use super::*;
use crate::capability::Unsupported;
use serde_json::json;

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

#[test]
fn commit_messages_come_from_where_each_forge_lists_them() {
    let forgejo = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
    let listed = [json!({"sha": "a", "commit": {"message": "Fix\n\nWhy"}})];
    assert_eq!(
        messages(&listed, forgejo.kind.commit_message()).unwrap(),
        ["Fix\n\nWhy"]
    );
    let gitlab = ForgeRepo::parse("git@gitlab.com:group/repo.git").unwrap();
    let listed = [json!({"id": "a", "message": "Fix"})];
    assert_eq!(
        messages(&listed, gitlab.kind.commit_message()).unwrap(),
        ["Fix"]
    );
    assert!(messages(&listed, forgejo.kind.commit_message()).is_err());
}
