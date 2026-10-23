//! Requests `shoal api` sends to a workspace repository's forge API, so a
//! scoped caller acts as its agent account and only reads unless the
//! repository lets agents write.
use anyhow::{Result, ensure};
use serde_json::Value;

use crate::{
    daemon::workspace::Manager,
    forge::{
        account::Account,
        raw::{HttpMethod, RawRequest},
    },
    model::Workspace,
};

impl Manager {
    /// The forge's response to `request` in the workspace's repository, sent
    /// as its agent account for a `scoped` caller and as the user's own
    /// logins otherwise.
    pub async fn forge_api(
        &self,
        selector: &str,
        request: &RawRequest,
        scoped: bool,
    ) -> Result<Value> {
        let workspace = self.workspace(selector).await?;
        self.verify_worktree(&workspace).await?;
        let forge = self.workspace_forge(&workspace).await?;
        let account = if scoped {
            self.api_agent_account(&workspace, request.method).await?
        } else {
            self.user_account()
        };
        forge.raw(&account, request).await
    }

    /// The agent account that may send a `method` request, which is only a
    /// read unless `[api] agent_writes` allows more.
    async fn api_agent_account(
        &self,
        workspace: &Workspace,
        method: HttpMethod,
    ) -> Result<Account> {
        // One snapshot, so a reload cannot pair a profile with another map.
        let config = self.config();
        let settings = self.workspace_settings_from(&config, workspace).await?;
        ensure!(
            method == HttpMethod::Get || settings.api.agent_writes,
            "workspace processes can only send GET requests; [api] agent_writes = true lets agents send {method}"
        );
        self.agent_account_from(&config, &settings)
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use serde_json::json;

    use crate::{
        forge::raw::{HttpMethod, RawRequest},
        test_support::{git, manager, repository},
    };

    #[tokio::test]
    async fn agents_read_as_their_account_and_write_only_when_allowed() {
        let (root, manager) = manager().await;
        let path = repository(root.path(), "repo");
        let repo = manager
            .register_repository(path.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "api".into(), None, None, None)
            .await
            .unwrap();
        git(
            &path,
            &["remote", "add", "origin", "https://github.com/team/repo"],
        );
        let gh = root.path().join("gh-agent");
        let log = root.path().join("log");
        std::fs::write(
            &gh,
            format!(
                "#!/bin/sh\necho \"$*\" >> '{}'\nprintf 'HTTP/2.0 200 OK\\n\\n{{\"ok\":true}}'\n",
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        let auth = format!("[agent_auth]\ngh = '{}'\n", gh.display());
        manager
            .set_repository_config(&repo.id, Some(auth.clone()))
            .await
            .unwrap();
        let request = |method| RawRequest {
            method,
            path: "repos/{owner}/{repo}".into(),
            body: None,
        };

        let read = manager
            .forge_api(&workspace.id, &request(HttpMethod::Get), true)
            .await
            .unwrap();
        assert_eq!(read, json!({"ok": true}));
        let error = manager
            .forge_api(&workspace.id, &request(HttpMethod::Patch), true)
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("agent_writes"), "{error:#}");
        let sent = std::fs::read_to_string(&log).unwrap();
        assert_eq!(sent.lines().count(), 1, "{sent}");
        assert!(sent.contains("-X GET repos/team/repo"), "{sent}");

        manager
            .set_repository_config(
                &repo.id,
                Some(format!("{auth}[api]\nagent_writes = true\n")),
            )
            .await
            .unwrap();
        manager
            .forge_api(&workspace.id, &request(HttpMethod::Patch), true)
            .await
            .unwrap();
        let sent = std::fs::read_to_string(&log).unwrap();
        assert!(sent.contains("-X PATCH repos/team/repo"), "{sent}");
    }
}
