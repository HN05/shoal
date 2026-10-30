use std::path::PathBuf;

use serde_json::json;
use tokio::process::Command;

use super::*;
use crate::account::{AgentLogins, Launch};
use crate::test_support::install_test_script;

/// Finds the CLIs only in one directory.
struct Bin(PathBuf);

impl Launch for Bin {
    fn apply(&self, command: &mut Command) {
        command.env("PATH", &self.0);
    }
}

#[tokio::test]
async fn requests_carry_their_method_and_body_and_return_the_json() {
    let dir = tempfile::tempdir().unwrap();
    let gh = dir.path().join("gh");
    install_test_script(
        &gh,
        &format!(
            // PATH holds only this script, so it reads its input with builtins.
            "#!/bin/sh\necho \"$*\" > '{0}/args'\nIFS= read -r body\n\
                 printf '%s' \"$body\" > '{0}/input'\n\
                 printf 'HTTP/2.0 200 OK\\n\\n{{\"sent\":true}}'\n",
            dir.path().display()
        ),
    );
    let logins = AgentLogins {
        launch: Box::new(Bin(dir.path().to_owned())),
        fj_home: None,
        fj: false,
        tea: false,
    };
    let account = Account::agent(logins, &Default::default());
    let repo = ForgeRepo::parse("git@github.com:team/repo.git").unwrap();
    let raw = RawRequest {
        method: HttpMethod::Patch,
        path: "repos/{owner}/{repo}".into(),
        body: Some(json!({"description": "new"})),
    };
    assert_eq!(
        repo.raw(&account, &raw).await.unwrap(),
        json!({"sent": true})
    );
    let args = std::fs::read_to_string(dir.path().join("args")).unwrap();
    assert!(
        args.contains("-X PATCH repos/team/repo --include"),
        "{args}"
    );
    let input = std::fs::read_to_string(dir.path().join("input")).unwrap();
    assert_eq!(input, r#"{"description":"new"}"#);
}

fn get(path: &str) -> RawRequest {
    RawRequest {
        method: HttpMethod::Get,
        path: path.into(),
        body: None,
    }
}

#[test]
fn placeholders_name_the_repository_on_every_forge() {
    for (remote, path, endpoint) in [
        (
            "git@github.com:team/repo.git",
            "/repos/{owner}/{repo}/branches?per_page=5",
            "repos/team/repo/branches?per_page=5",
        ),
        (
            "https://forge.example/team/repo",
            "repos/{owner}/{repo}/actions/runs",
            "repos/team/repo/actions/runs",
        ),
        // GitLab names a nested project by its encoded full path.
        (
            "git@gitlab.com:group/sub/project.git",
            "projects/{owner}%2F{repo}/pipelines",
            "projects/group%2Fsub%2Fproject/pipelines",
        ),
        (
            "git@bitbucket.org:workspace/repo.git",
            "repositories/{owner}/{repo}/refs",
            "repositories/workspace/repo/refs",
        ),
        ("git@github.com:team/repo.git", "user", "user"),
    ] {
        let repo = ForgeRepo::parse(remote).unwrap();
        assert_eq!(repo.raw_endpoint(&get(path)).unwrap(), endpoint, "{remote}");
    }
}

#[test]
fn paths_stay_under_the_api_root() {
    let repo = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
    for path in [
        "",
        "/",
        "?page=2",
        "https://other.example/api/v1/user",
        "repos/../../user/settings",
        "repos/./team",
        "repos/%2e%2E/user",
        "repos/.%2e",
        "repos/..%2f..%2fuser",
        "repos/..%5Cuser",
        "repos/%252e%252e/user",
        "repos/team/repo issues",
        "repos/team\n",
    ] {
        assert!(get(path).check().is_err(), "{path:?}");
    }
    // A query or fragment may hold dots.
    assert_eq!(
        repo.raw_endpoint(&get("repos/team/repo/contents?ref=.."))
            .unwrap(),
        "repos/team/repo/contents?ref=.."
    );
    // Encoded separators that name no dot segment stay, as GitLab needs.
    assert!(get("projects/group%2Fsub%2Fproject").check().is_ok());
    assert!(get("repos/team/repo/contents/a%2E%2Eb").check().is_ok());
}
