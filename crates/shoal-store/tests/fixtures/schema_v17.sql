-- Schema produced by store::migrate before the ordered-step refactor.
CREATE TABLE repositories (
            id TEXT PRIMARY KEY, path TEXT NOT NULL UNIQUE, source TEXT NOT NULL, last_used INTEGER NOT NULL
        , name TEXT, workspaces_dir TEXT);
CREATE TABLE workspaces (
            id TEXT PRIMARY KEY, repository_id TEXT NOT NULL REFERENCES repositories(id),
            name TEXT NOT NULL UNIQUE, path TEXT NOT NULL UNIQUE, branch TEXT NOT NULL,
            state TEXT NOT NULL, error TEXT
        , base_commit TEXT, base_ref TEXT, git_dir TEXT, git_dir_id TEXT, setup_finished INTEGER NOT NULL DEFAULT 0 CHECK(setup_finished IN (0,1)));
CREATE TABLE executions (
            id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL REFERENCES workspaces(id), state TEXT NOT NULL
        , wrapper TEXT, child TEXT, group_id INTEGER);
CREATE TABLE ports (
            workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
            name TEXT NOT NULL, port INTEGER NOT NULL UNIQUE CHECK(port BETWEEN 1 AND 65535),
            env_var TEXT NOT NULL, reason TEXT, PRIMARY KEY(workspace_id, name), UNIQUE(workspace_id, env_var)
        );
CREATE UNIQUE INDEX repository_names ON repositories(name) WHERE name IS NOT NULL;
CREATE TABLE simulators(id TEXT PRIMARY KEY, record TEXT NOT NULL);
CREATE TABLE simulator_clean_requests (
            id INTEGER PRIMARY KEY AUTOINCREMENT, request_id TEXT NOT NULL UNIQUE,
            workspace_id TEXT NOT NULL, record TEXT NOT NULL
        );
CREATE INDEX clean_requests_workspace ON simulator_clean_requests(workspace_id,id);
CREATE TABLE resource_pools (
            scope TEXT NOT NULL, name TEXT NOT NULL, definition TEXT NOT NULL, PRIMARY KEY(scope,name)
        );
CREATE TABLE resource_leases (
            id TEXT PRIMARY KEY, workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
            scope TEXT NOT NULL, pool TEXT NOT NULL, name TEXT NOT NULL, resource TEXT NOT NULL,
            reason TEXT, created_at INTEGER NOT NULL, mode TEXT NOT NULL DEFAULT 'permit' CHECK(mode IN ('permit','read','write')), UNIQUE(workspace_id,pool,name),
            FOREIGN KEY(scope,pool) REFERENCES resource_pools(scope,name)
        );
CREATE INDEX resource_lease_pool ON resource_leases(scope,pool);
CREATE TABLE access_requests (
            id TEXT PRIMARY KEY,
            workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
            target_key TEXT NOT NULL, name TEXT NOT NULL,
            record TEXT NOT NULL, active INTEGER NOT NULL DEFAULT 1 CHECK(active IN (0,1))
        );
CREATE UNIQUE INDEX access_request_name
            ON access_requests(workspace_id,target_key,name) WHERE active=1;
CREATE TABLE repository_removals (
            repository_id TEXT PRIMARY KEY REFERENCES repositories(id) ON DELETE CASCADE,
            directory_id TEXT,
            deleting_files INTEGER NOT NULL DEFAULT 0 CHECK(deleting_files IN (0,1))
        );
CREATE TABLE repository_configs (
            repository_id TEXT PRIMARY KEY REFERENCES repositories(id) ON DELETE CASCADE,
            toml TEXT NOT NULL
        );
CREATE TABLE pr_cleanup (
            workspace_id TEXT PRIMARY KEY REFERENCES workspaces(id) ON DELETE CASCADE,
            record TEXT NOT NULL
        );
CREATE TABLE notifications (
            id INTEGER PRIMARY KEY AUTOINCREMENT, created_at INTEGER NOT NULL, workspace TEXT,
            kind TEXT NOT NULL, message TEXT NOT NULL, read INTEGER NOT NULL DEFAULT 0 CHECK(read IN (0,1))
        );
CREATE INDEX notifications_unread ON notifications(read,id);
PRAGMA user_version=17;
