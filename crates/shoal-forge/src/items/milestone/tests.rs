use super::*;
use crate::test_support::install_test_script;
use crate::{
    account::{AgentLogins, Launch},
    backends::Backend,
    transport::memory::Memory,
};
use serde_json::json;
use std::path::PathBuf;
use tokio::process::Command;

/// Finds the CLIs only in one directory.
struct Bin(PathBuf);

impl Launch for Bin {
    fn apply(&self, command: &mut Command) {
        command.env("PATH", &self.0);
    }
}

#[test]
fn each_forge_lists_milestones_and_names_their_ids_its_way() {
    let listed = json!({"id": 101, "number": 5, "title": "v2", "name": "v2"});
    let github = ForgeRepo::parse("https://github.com/team/repo").unwrap();
    let forgejo = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
    let gitlab = ForgeRepo::parse("git@gitlab.com:group/sub/repo.git").unwrap();
    let bitbucket = ForgeRepo::parse("git@bitbucket.org:workspace/repo.git").unwrap();
    assert_eq!(github.kind.milestone(&listed), Some(("v2".into(), 5)));
    assert_eq!(forgejo.kind.milestone(&listed), Some(("v2".into(), 101)));
    assert_eq!(gitlab.kind.milestone(&listed), Some(("v2".into(), 101)));
    assert_eq!(
        bitbucket.kind.milestone(&json!({"id": 3, "name": "v2"})),
        Some(("v2".into(), 3))
    );
    assert_eq!(forgejo.kind.milestone(&json!({"title": "v2"})), None);
    assert_eq!(
        github.kind.milestones_endpoint(&github),
        "repos/team/repo/milestones?state=all"
    );
    assert_eq!(
        gitlab.kind.milestones_endpoint(&gitlab),
        "projects/group%2Fsub%2Frepo/milestones?include_ancestors=true"
    );
    assert_eq!(
        bitbucket.kind.milestones_endpoint(&bitbucket),
        "repositories/workspace/repo/milestones"
    );
}

#[tokio::test]
async fn a_milestone_is_found_by_its_exact_title() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("log");
    install_test_script(
        &dir.path().join("gh"),
        &format!(
            r#"#!/bin/sh
echo "$*" >> '{log}'
printf 'HTTP/2.0 200 OK\n\n[{{"number":4,"title":"V2"}},{{"number":5,"title":"v2"}}]'
"#,
            log = log.display()
        ),
    );
    let logins = AgentLogins {
        launch: Box::new(Bin(dir.path().to_owned())),
        logins: Default::default(),
        separate: Vec::new(),
    };
    let account = Account::agent(logins, Default::default()).remembering(Memory::default());
    let github = ForgeRepo::parse("https://github.com/team/repo").unwrap();
    assert_eq!(github.milestone_id(&account, "v2").await.unwrap(), 5);
    assert_eq!(
        github
            .milestone_id(&account, "v3")
            .await
            .unwrap_err()
            .to_string(),
        "team/repo has no milestone v3"
    );
    let log = std::fs::read_to_string(log).unwrap();
    assert!(
        log.contains("repos/team/repo/milestones?state=all"),
        "{log}"
    );
}
