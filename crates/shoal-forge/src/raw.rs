//! Any request to the forge's API, for reads no other operation covers.
//! Whether a caller may send one that changes something is the caller's
//! decision.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use crate::transport::api::HttpMethod;
use crate::{
    ForgeRepo,
    account::Account,
    transport::api::{Request, encode},
};

/// A request to a path under the forge's API root, such as
/// `repos/{owner}/{repo}/branches`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawRequest {
    pub method: HttpMethod,
    pub path: String,
    pub body: Option<Value>,
}

impl ForgeRepo {
    /// The forge's JSON response to `raw`, sent as `account`; null for an
    /// empty one.
    pub async fn raw(&self, account: &Account, raw: &RawRequest) -> Result<Value> {
        let endpoint = self.raw_endpoint(&raw.path)?;
        let mut request = Request::new(raw.method, endpoint, raw.body.clone());
        if raw.method == HttpMethod::Get {
            request = request.repeatable();
        }
        self.send(account, &request).await
    }

    /// `path` relative to the API root, with `{owner}` and `{repo}` filled in
    /// from this repository. It stays under the root, so it cannot name
    /// another host or climb out of the API.
    fn raw_endpoint(&self, path: &str) -> Result<String> {
        let path = path.trim_start_matches('/');
        let route = path.split(['?', '#']).next().unwrap_or_default();
        ensure!(!route.is_empty(), "the API path is empty");
        ensure!(
            !path.contains("://") && !path.chars().any(|c| c.is_whitespace() || c.is_control()),
            "the API path must be relative to the forge's API root, without spaces"
        );
        ensure!(
            route.split('/').all(|part| !matches!(part, "." | "..")),
            "the API path must not contain `.` or `..` segments"
        );
        let (owner, repo) = self.path.rsplit_once('/').unwrap_or(("", &self.path));
        Ok(path
            .replace("{owner}", &encode(owner, false))
            .replace("{repo}", &encode(repo, false)))
    }
}

#[cfg(test)]
mod tests {
    use std::{os::unix::fs::PermissionsExt, path::PathBuf};

    use serde_json::json;
    use tokio::process::Command;

    use super::*;
    use crate::account::{AgentLogins, Launch};

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
        std::fs::write(
            &gh,
            format!(
                // PATH holds only this script, so it reads its input with builtins.
                "#!/bin/sh\necho \"$*\" > '{0}/args'\nIFS= read -r body\n\
                 printf '%s' \"$body\" > '{0}/input'\n\
                 printf 'HTTP/2.0 200 OK\\n\\n{{\"sent\":true}}'\n",
                dir.path().display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
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
            assert_eq!(repo.raw_endpoint(path).unwrap(), endpoint, "{remote}");
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
            "repos/team/repo issues",
            "repos/team\n",
        ] {
            assert!(repo.raw_endpoint(path).is_err(), "{path:?}");
        }
        // A query or fragment may hold dots.
        assert_eq!(
            repo.raw_endpoint("repos/team/repo/contents?ref=..")
                .unwrap(),
            "repos/team/repo/contents?ref=.."
        );
    }
}
