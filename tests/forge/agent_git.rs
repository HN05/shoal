//! An agent's Git over HTTPS to a host with an agent token, against the
//! scripted forge's HTTPS server.
use std::fs;

use serde_json::json;

use super::{Fixture, Forge};
use crate::support::{forge::Server, git, printed};

/// An agent's push to a host with an agent token signs in with the token,
/// though the user's `.netrc` holds a login of their own for the host,
/// which libcurl would send before Git asks a credential helper.
#[test]
fn an_agent_pushes_with_its_token_over_the_users_netrc_login() {
    let fixture = Fixture::new(Forge::Forgejo, "");
    let root = fixture.root.path();
    let https = Server::start_tls(root);
    let config = fs::read_to_string(root.join(".config/shoal/config.toml")).unwrap();
    fixture.write_config(&format!(
        "{config}\n[machine.forges.\"{}\"]\ntype = \"forgejo\"\nagent_token = \"agenttoken\"\n",
        https.host
    ));
    fixture.ok(&["config", "reload"]);
    let remote = root.join("forge/git/team/project.git");
    git(
        root,
        &["init", "--quiet", "--bare", remote.to_str().unwrap()],
    );
    fs::write(
        root.join(".netrc"),
        "machine 127.0.0.1 login netrc password usernetrc\n",
    )
    .unwrap();
    let push = format!(
        "GIT_SSL_CAINFO={} git push --quiet https://{}/team/project.git HEAD:refs/heads/topic",
        Server::certificate(root),
        https.host
    );
    fs::write(
        fixture.topic().join(".shoal.toml"),
        format!("[agents.claude]\ncommand = ['sh', '-c', '{push}']\n"),
    )
    .unwrap();

    let agent = fixture.run(&[
        "start",
        "--in",
        &fixture.id("topic"),
        "--here",
        "--agent",
        "claude",
    ]);

    assert!(agent.status.success(), "{}", printed(&agent));
    let logins: Vec<_> = (fixture.requests().into_iter())
        .filter(|request| request[0] == "PUSH team/project")
        .map(|request| request[2].clone())
        .collect();
    assert_eq!(logins, [json!("token:agenttoken")]);
}
