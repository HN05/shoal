use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use super::*;

/// A token that counts how often it is fetched.
struct Counted {
    token: &'static str,
    fetches: Arc<AtomicUsize>,
}

impl Token for Counted {
    fn fetch(&self) -> BoxFuture<'_, Result<String>> {
        self.fetches.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { Ok(self.token.to_owned()) })
    }
}

fn logins(hosts: &[(&str, &'static str)], fetches: &Arc<AtomicUsize>) -> Logins {
    let mut logins = Logins::default();
    for (host, token) in hosts {
        let token = Box::new(Counted {
            token,
            fetches: fetches.clone(),
        });
        logins.insert(host, Login { token, email: None });
    }
    logins
}

fn repo(host: &str) -> ForgeRepo {
    ForgeRepo::parse(&format!("https://{host}/team/project")).unwrap()
}

async fn header(account: &Account, host: &str) -> Result<Option<String>> {
    let header = account.authorization(&repo(host)).await?;
    Ok(header.map(|bytes| String::from_utf8(bytes).unwrap()))
}

#[tokio::test]
async fn agents_use_their_own_token_then_the_users() {
    let fetches = Arc::new(AtomicUsize::new(0));
    let user = logins(
        &[("agent.example", "user-a"), ("user.example", "user-u")],
        &fetches,
    );
    let agent = logins(&[("Agent.Example", "agent-a")], &fetches);
    let account = Account::agent(agent, user);
    assert_eq!(
        header(&account, "agent.example").await.unwrap().unwrap(),
        "header = \"Authorization: token agent-a\"\n"
    );
    assert!(
        header(&account, "user.example")
            .await
            .unwrap()
            .unwrap()
            .contains("user-u")
    );
    assert_eq!(header(&account, "none.example").await.unwrap(), None);
    // Each login's token is fetched once for the account's requests.
    header(&account, "agent.example").await.unwrap();
    assert_eq!(fetches.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn malformed_tokens_are_refused() {
    let fetches = Arc::new(AtomicUsize::new(0));
    for token in ["", " ", "a b", "a\"b", "a\\b", "a\nb"] {
        let account = Account::user(logins(&[("forge.example", token)], &fetches));
        assert!(
            header(&account, "forge.example").await.is_err(),
            "{token:?}"
        );
    }
    let padded = Account::user(logins(&[("forge.example", " abc\n")], &fetches));
    assert!(
        header(&padded, "forge.example")
            .await
            .unwrap()
            .unwrap()
            .contains("token abc\"")
    );
}

#[tokio::test]
async fn each_forge_signs_in_with_its_own_scheme_from_one_fetch() {
    let fetches = Arc::new(AtomicUsize::new(0));
    let account = Account::user(logins(&[("forge.example", "abc")], &fetches));
    let on = |kind| ForgeRepo::host("forge.example", "https", kind);
    let header = |bytes: Option<Vec<u8>>| String::from_utf8(bytes.unwrap()).unwrap();
    let github = account.authorization(&on(ForgeKind::GitHub)).await.unwrap();
    let forgejo = account
        .authorization(&on(ForgeKind::Forgejo))
        .await
        .unwrap();
    assert!(header(github).contains("Bearer abc"));
    assert!(header(forgejo).contains("token abc"));
    assert_eq!(fetches.load(Ordering::SeqCst), 1);
}

#[test]
fn changes_without_a_login_name_the_missing_token() {
    let fetches = Arc::new(AtomicUsize::new(0));
    let user = logins(&[("user.example", "user-u")], &fetches);
    let account = Account::agent(Logins::default(), user)
        .naming_missing(Box::new(|host| format!("set a token for {host}")));
    // The user's login stands in for the agent's own.
    account.require_login(&repo("user.example")).unwrap();
    let refused = account.require_login(&repo("none.example")).unwrap_err();
    assert_eq!(refused.to_string(), "set a token for none.example");
    let unnamed = Account::default().require_login(&repo("none.example"));
    assert_eq!(
        unnamed.unwrap_err().to_string(),
        "no token for none.example"
    );
    assert_eq!(fetches.load(Ordering::SeqCst), 0);
}
