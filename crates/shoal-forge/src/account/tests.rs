use super::*;
use crate::test_support::install_test_script;

/// An agent with `fj_home`, whose fj has a login for `fj.example` only,
/// and a `tea` wrapper with a login for `tea.example` only.
fn agent(root: &std::path::Path) -> Account {
    let home = root.join("fj-home");
    let keys = fj_keys_at(&home, None);
    std::fs::create_dir_all(keys.parent().unwrap()).unwrap();
    std::fs::write(
        &keys,
        r#"{"hosts":{"fj.example":{"type":"Application","token":"fj-token"}}}"#,
    )
    .unwrap();
    let bin = root.join("bin");
    std::fs::create_dir(&bin).unwrap();
    let tea = bin.join("tea");
    install_test_script(
        &tea,
        "#!/bin/sh\necho '[{\"name\":\"agent\",\"url\":\"https://tea.example\"}]'\n",
    );
    let logins = AgentLogins {
        launch: Box::new(Bin(bin)),
        fj_home: Some(home),
        fj: false,
        tea: true,
    };
    Account::agent(logins, &Default::default())
}

/// Finds the CLIs only in one directory.
struct Bin(std::path::PathBuf);

impl Launch for Bin {
    fn apply(&self, command: &mut tokio::process::Command) {
        command.env("PATH", &self.0);
    }
}

#[tokio::test]
async fn agents_fall_back_from_fj_to_tea_per_host() {
    let root = tempfile::tempdir().unwrap();
    let account = agent(root.path());
    assert!(matches!(
        account.forgejo_login("fj.example").await.unwrap(),
        ForgejoLogin::Token(token) if token == "fj-token"
    ));
    assert!(matches!(
        account.forgejo_login("tea.example").await.unwrap(),
        ForgejoLogin::Tea(login) if login.name == "agent"
    ));
    let error = account.forgejo_login("other.example").await.err().unwrap();
    assert!(
        format!("{error:#}").contains("the agent's fj login"),
        "{error:#}"
    );
}
