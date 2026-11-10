//! The forge: reviews, linked items and their watches, PR cleanup,
//! restacking and `shoal view`.
use super::chats::start_test_chat;
use super::*;

/// GitHub's search request for `query`, page `page`.
fn github_search(query: &str, page: u32) -> String {
    let query: String = query
        .bytes()
        .map(|byte| match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(byte).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect();
    format!("GET search/issues?q={query}&sort=created&order=desc&per_page=100&page={page}")
}

/// GitHub's answers listing `issues` and `pulls` as team/project's open
/// items, each PR from its `topic` branch.
pub(crate) fn github_open_items(
    issues: &[(u64, &str)],
    pulls: &[(u64, &str)],
) -> serde_json::Map<String, Value> {
    let items = |items: &[(u64, &str)]| -> Vec<Value> {
        items
            .iter()
            .map(|(number, title)| {
                serde_json::json!({"number": number, "title": title, "state": "open", "labels": []})
            })
            .collect()
    };
    let mut responses = serde_json::Map::new();
    responses.insert(
        github_search("repo:team/project is:issue is:open", 1),
        serde_json::json!({"body": {"items": items(issues)}}),
    );
    responses.insert(
        github_search("repo:team/project is:pr is:open", 1),
        serde_json::json!({"body": {"items": items(pulls)}}),
    );
    for (number, title) in pulls {
        responses.insert(
            format!("GET repos/team/project/pulls/{number}"),
            serde_json::json!({"body": {"number": number, "title": title, "state": "open",
                "body": "", "labels": [], "draft": false,
                "head": {"ref": "topic", "sha": "head", "repo": {"full_name": "team/project"}},
                "base": {"ref": "main"}}}),
        );
    }
    responses
}

/// The REST answers for open issue `number` of GitHub's team/project,
/// titled `title` and described by `body`, which no issue blocks.
pub(crate) fn github_issue(number: u64, title: &str, body: &str) -> serde_json::Map<String, Value> {
    let issue = format!("repos/team/project/issues/{number}");
    let responses = serde_json::json!({
        format!("GET {issue}"): {"body": {"number": number, "title": title, "state": "open",
            "body": body, "labels": []}},
        format!("GET {issue}/dependencies/blocked_by?per_page=100&page=1"): {"body": []},
    });
    responses.as_object().unwrap().clone()
}

/// The REST answers for open PR `number` of `branch` on GitHub's
/// team/project and its activity, which has none.
fn github_pr(number: u64, branch: &str) -> serde_json::Map<String, Value> {
    let pull = format!("repos/team/project/pulls/{number}");
    let commit = "repos/team/project/commits/head";
    let responses = serde_json::json!({
        format!("GET {pull}"): {"body": {"number": number, "title": "PR", "state": "open",
            "body": "", "labels": [], "draft": false, "mergeable": true,
            "head": {"ref": branch, "sha": "head", "repo": {"full_name": "team/project"}},
            "base": {"ref": "main", "sha": "base"}}},
        format!("GET repos/team/project/issues/{number}/comments?per_page=100&page=1"): {"body": []},
        format!("GET {pull}/reviews?per_page=100&page=1"): {"body": []},
        format!("GET {pull}/comments?per_page=100&page=1"): {"body": []},
        format!("GET {commit}/check-runs?filter=latest&per_page=100"):
            {"body": {"total_count": 0, "check_runs": []}},
        format!("GET {commit}/status?per_page=100"): {"body": {"total_count": 0, "statuses": []}},
    });
    responses.as_object().unwrap().clone()
}

/// The answer listing `runs` as the check runs on the head commit of a
/// [`github_pr`].
fn github_check_runs(runs: Value) -> (String, Value) {
    let count = runs.as_array().unwrap().len();
    (
        "GET repos/team/project/commits/head/check-runs?filter=latest&per_page=100".into(),
        serde_json::json!({"body": {"total_count": count, "check_runs": runs}}),
    )
}

#[test]
fn review_runs_the_configured_command_or_queues_an_agent_review() {
    let fixture = Fixture::with_config(Some(
        "agent.default = 'reviewer'\n[commands]\nreview = ['printf', 'manual %s', '{branch}']\n[agents.reviewer]\ncommand = ['printf', '%s|', '{prompt}', '{args}']\n[agents.absent]\ncommand = ['no-such-reviewer']\nreview = ['{prompt}', '{schema}']\n",
    ));
    fixture.add("changes");
    assert!(!fixture.run(&["review", "run", "changes"]).status.success());
    assert_eq!(
        fixture
            .run(&["review", "run", "changes", "--manual"])
            .stdout,
        b"manual changes"
    );
    for args in [
        &[
            "review", "run", "changes", "--agent", "reviewer", "--", "extra",
        ][..],
        &["review", "run", "changes", "--agent", "reviewer"],
        &["review", "run", "changes", "--agent", "absent"],
    ] {
        let output = fixture.run(args);
        assert!(!output.status.success(), "{output:?}");
    }
    // Nothing was queued.
    assert_eq!(
        fixture.ok(&["review", "ls", "changes"]),
        serde_json::json!([])
    );
    // Without a configured review command, the agent reviews in the daemon.
    let result = fixture.root.path().join("review.json");
    fs::write(
        &result,
        r#"{"summary":"One nit.","verdict":"comment","checked":[],"findings":[{"path":"tracked","line":1,"severity":"nit","title":"Name it","body":"Say what it holds."}]}"#,
    )
    .unwrap();
    let fixture = Fixture::with_config(Some(&format!(
        "agent.default = 'reviewer'\n[agents.reviewer]\ncommand = ['sh']\nreview = ['-c', 'cat {}', '{{prompt}}', '{{schema}}']\n",
        result.display()
    )));
    fixture.add("agent-only");
    // The daemon may start it before replying, so its state is not checked.
    let queued = fixture.ok(&["review", "run", "agent-only"]);
    let id = queued["id"].to_string();
    let review = fixture.ok(&["watch", "review", &id]);
    assert_eq!(review["state"], "finished", "{review}");
    assert_eq!(review["reviewer"], "reviewer");
    let finding = format!("{id}.1");
    assert_eq!(review["findings"][0]["id"], finding);
    let resolved = fixture.ok(&[
        "review",
        "resolve",
        &finding,
        "false-positive",
        "--reason",
        "The name says it",
    ]);
    assert_eq!(
        resolved["findings"][0]["outcome"],
        serde_json::json!({"outcome": "false-positive", "reason": "The name says it"})
    );
    let listed = fixture.ok(&["review", "ls", "agent-only"]);
    assert_eq!(listed[0]["id"].to_string(), id);
    assert_eq!(fixture.ok(&["view", "review", &id])["summary"], "One nit.");
    let workspaces = fixture.ok(&["work"]);
    assert_eq!(workspaces[0]["latest_review"]["status"], "finished: 1 nit");
    // Plain view shows it in a repository without a forge, with nothing linked.
    let viewed = fixture.ok(&["view", "--space", "agent-only"]);
    assert_eq!(viewed, serde_json::json!([viewed[0]]), "{viewed}");
    assert_eq!(viewed[0]["kind"], "review", "{viewed}");
    assert_eq!(viewed[0]["id"].to_string(), id);
}

#[test]
fn a_review_watch_outlasts_a_daemon_restart() {
    let files = tempfile::tempdir().unwrap();
    let result = files.path().join("review.json");
    fs::write(
        &result,
        r#"{"summary":"No issues found.","verdict":"approve","checked":[],"findings":[]}"#,
    )
    .unwrap();
    // The reviewer waits for the gate, for at most a minute.
    let gate = files.path().join("gate");
    let script = format!(
        "i=0; while [ ! -e {} ] && [ $i -lt 600 ]; do sleep 0.1; i=$((i+1)); done; cat {}",
        gate.display(),
        result.display()
    );
    let mut fixture = Fixture::with_config(Some(&format!(
        "agent.default = 'reviewer'\n[agents.reviewer]\ncommand = ['sh']\nreview = ['-c', '{script}', '{{prompt}}', '{{schema}}']\n"
    )));
    fixture.add("watched");
    let id = fixture.ok(&["review", "run", "watched"])["id"].to_string();
    wait_until("the review to run", || {
        fixture.ok(&["view", "review", &id])["state"] == "running"
    });
    let mut watch = fixture
        .command()
        .args(["--json", "watch", "review", &id])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // The restart stops the reviewer; the review starts over and the watch
    // waits on.
    fixture.restart();
    fs::write(&gate, "").unwrap();
    if try_wait_for(|| watch.try_wait().unwrap()).is_none() {
        let _ = watch.kill();
    }
    let output = watch.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let review: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(review["state"], "finished", "{review}");
}

/// An upstream remote served at ssh://git@forge.example/team/project.git;
/// returns the author's clone.
pub(crate) fn forge_origin(fixture: &Fixture) -> PathBuf {
    let author = upstream_remote(fixture);
    let remote = fixture.root.path().join("origin.git");
    let ssh = fixture.root.path().join("ssh");
    install_test_script(
        &ssh,
        format!(
            "#!/bin/sh\nfor last; do :; done\nexec sh -c \"$(printf '%s' \"$last\" | sed 's|/team/project.git|{}|')\"\n",
            remote.display()
        ),
    );
    git(
        &fixture.repo,
        &["config", "core.sshCommand", ssh.to_str().unwrap()],
    );
    git(
        &fixture.repo,
        &[
            "remote",
            "set-url",
            "origin",
            "ssh://git@forge.example/team/project.git",
        ],
    );
    author
}

#[test]
fn review_refers_issues_to_plan_mode() {
    let fixture = Fixture::with_config(Some(
        "agent.default = 'reviewer'\n[agents.reviewer]\ncommand = ['printf', '%s', '{prompt}']\n",
    ));
    forge_origin(&fixture);
    let output = fixture
        .command()
        .args([
            "review",
            "run",
            "https://forge.example/team/project/issues/12",
        ])
        .current_dir(&fixture.repo)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let output = fixture
        .command()
        .args(["review", "run", "--issue", "12"])
        .current_dir(&fixture.repo)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(fixture.ok(&["work"]), serde_json::json!([]));
}

#[test]
fn a_chat_reviews_its_linked_pr_and_lists_and_cancels_that_review() {
    let fixture = Fixture::with_config(Some(
        "agent.default = 'reviewer'\n[agents.reviewer]\ncommand = ['sh']\nreview = ['-c', 'exit 1', '{prompt}', '{schema}']\n",
    ));
    let author = forge_origin(&fixture);
    git(&author, &["switch", "-c", "fix"]);
    fs::write(author.join("fix"), "change\n").unwrap();
    git(&author, &["add", "fix"]);
    git(
        &author,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "fix",
        ],
    );
    git(&author, &["push", "origin", "fix"]);
    let bin = fixture.root.path().join("bin");
    install_api_curl(&bin);
    let mut pull = forgejo_pull(7, "open", "fix", &"7".repeat(40));
    pull["head"]["repo"]["full_name"] = "team/project".into();
    script_api(fixture.root.path(), "repos/team/project/pulls/7", &pull);
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let (mut agent, _) = start_test_chat(&fixture, "planning");
    let run = |args: &[&str]| {
        fixture
            .command()
            .arg("--json")
            .args(args)
            .current_dir(&fixture.repo)
            .env("PATH", &path)
            .output()
            .unwrap()
    };
    // Nothing linked: nothing to review.
    let output = run(&["review", "run", "planning", "--agent", "reviewer"]);
    assert!(!output.status.success(), "{output:?}");
    let output = run(&["link", "pr", "7", "assigned", "--space", "planning"]);
    assert!(output.status.success(), "{output:?}");
    // The linked PR is reviewed in the workspace that owns its head, and
    // posting applies to it.
    let output = run(&["review", "run", "planning", "--no-post"]);
    assert!(output.status.success(), "{output:?}");
    let ended = fixture.ok(&["watch", "review", "1"]);
    let url = "https://forge.example/team/project/pulls/7";
    assert_eq!(ended["target"]["url"], url, "{ended}");
    assert_eq!(ended["target"]["post"], false, "{ended}");
    let listed = fixture.ok(&["review", "ls", "planning"]);
    assert_eq!(listed[0]["id"], 1, "{listed}");
    let output = run(&["review", "cancel", "--space", "planning"]);
    assert!(!output.status.success(), "{output:?}");
    agent.kill().unwrap();
    agent.wait().unwrap();
}

#[test]
fn review_opens_a_pr_head_against_its_base_and_reuses_the_owner() {
    let fixture = Fixture::with_config(Some(
        "agent.default = 'reviewer'\n[agents.reviewer]\ncommand = ['sh']\nreview = ['-c', 'exit 1', '{prompt}', '{schema}']\n",
    ));
    let author = forge_origin(&fixture);
    let commit = |message: &str| {
        fs::write(author.join(message), "change\n").unwrap();
        git(&author, &["add", message]);
        git(
            &author,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-m",
                message,
            ],
        );
    };
    git(&author, &["switch", "-c", "stack/base"]);
    commit("base");
    git(&author, &["switch", "-c", "stack/top"]);
    commit("top");
    git(&author, &["push", "origin", "stack/base", "stack/top"]);

    // The daemon also queries the PR when linking it, through the fixture's bin.
    let bin = fixture.root.path().join("bin");
    install_api_curl(&bin);
    let mut pull = forgejo_pull(7, "open", "stack/top", &"7".repeat(40));
    pull["title"] = "Top change".into();
    pull["base"]["ref"] = "stack/base".into();
    pull["head"]["repo"]["full_name"] = "team/project".into();
    script_api(fixture.root.path(), "repos/team/project/pulls/7", &pull);
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let review = |target: &[&str]| {
        fixture
            .command()
            .args(["review", "run"])
            .args(target)
            .args(["--agent", "reviewer"])
            .current_dir(&fixture.repo)
            .env("PATH", &path)
            .output()
            .unwrap()
    };
    let output = review(&["--pr", "7"]);
    assert!(output.status.success(), "{output:?}");
    // The review covers the PR's head against its base, both from origin.
    let ended = fixture.ok(&["watch", "review", "1"]);
    assert_eq!(ended["target"]["head"], "stack/top", "{ended}");
    assert_eq!(ended["target"]["base"], "stack/base", "{ended}");
    assert_eq!(
        ended["head"],
        git(&author, &["rev-parse", "stack/top"]).trim()
    );
    assert_eq!(ended["failure"], "execution-failed");
    assert!(
        fs::read_to_string(fixture.root.path().join("curl-urls"))
            .unwrap()
            .contains("https://forge.example/api/v1/repos/team/project/pulls/7\n")
    );
    let url = "https://forge.example/team/project/pulls/7";
    let linked_prs =
        || fixture.ok(&["work", "status", "stack-top"])["workspace"]["links"]["prs"].clone();
    assert_eq!(linked_prs(), serde_json::json!([url]));
    // Opening the PR again relinks it to its owner.
    fixture.ok(&["unlink", "pr", url, "assigned", "--space", "stack-top"]);
    assert_eq!(linked_prs(), serde_json::json!([]));
    let added = fixture
        .command()
        .args([
            "--json",
            "start",
            "https://forge.example/team/project/pulls/7",
        ])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(added.status.success(), "{added:?}");
    assert_eq!(linked_prs(), serde_json::json!([url]));
    let added: Value = serde_json::from_slice(&added.stdout).unwrap();
    assert_eq!(added["branch"], "stack/top");
    assert_eq!(added["links"]["prs"], serde_json::json!([url]));
    let branch = fixture.ok(&[
        "start",
        "https://forge.example/team/project/src/branch/stack%2Ftop",
    ]);
    assert_eq!(branch["id"], added["id"]);
    let workspace = &fixture.ok(&["work", "status", "stack-top"])["workspace"];
    assert_eq!(workspace["base_ref"], "refs/remotes/origin/stack/base");
    let path_in = Path::new(workspace["path"].as_str().unwrap());
    assert_eq!(
        git(
            path_in,
            &["merge-base", "--fork-point", "origin/stack/base", "HEAD"]
        ),
        git(&author, &["rev-parse", "stack/base"])
    );

    // Reviewing in the owner links the PR as well.
    fixture.ok(&["unlink", "pr", url, "assigned", "--space", "stack-top"]);
    commit("more");
    git(&author, &["push", "origin", "stack/top"]);
    let output = review(&["https://forge.example/team/project/pulls/7", "--no-post"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(linked_prs(), serde_json::json!([url]));
    assert_eq!(
        fixture.ok(&["watch", "review", "2"])["target"]["post"],
        false
    );
    assert!(path_in.join("more").exists());
    // A diverged workspace is left for the reviewer to reconcile.
    git(path_in, &["reset", "--hard", "HEAD~1"]);
    fs::write(path_in.join("local"), "change\n").unwrap();
    git(path_in, &["add", "local"]);
    git(
        path_in,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "local",
        ],
    );
    assert!(!review(&["--pr", "7"]).status.success());
    let output = review(&["https://forge.example/other/project/pulls/7"]);
    assert!(!output.status.success());
}

#[test]
fn link_picks_an_omitted_open_issue_or_pull_request() {
    let fixture = Fixture::new();
    fixture.add("topic");
    fixture.add_github_origin();
    let mut responses = github_open_items(&[(34, "Fix API timeout")], &[(7, "Topic")]);
    responses.extend(github_issue(34, "Fix API timeout", ""));
    fixture.script_github(&responses);
    let missing = fixture.run(&["link", "assigned", "--space", "topic"]);
    assert!(!missing.status.success());
    let picker = fixture.root.path().join("bin/fzf");
    for (args, kind, number) in [
        (
            vec!["link", "assigned", "--space", "topic"],
            "Pull request",
            "7",
        ),
        (
            vec!["link", "issue", "assigned", "--space", "topic"],
            "",
            "34",
        ),
    ] {
        install_test_script(
            &picker,
            format!("#!/bin/sh\nawk -F '\\t' '$2 == \"{kind}\" || $1 == \"{number}\" {{print}}'\n"),
        );
        let (output, transcript) = fixture.interactive_command(fixture.command().args(&args), "");
        assert!(output.status.success(), "{output:?} {transcript}");
    }
    let workspace = fixture.ok(&["work", "status", "topic"]);
    assert_eq!(
        workspace["workspace"]["links"]["prs"],
        serde_json::json!(["https://github.com/team/project/pull/7"])
    );
    assert_eq!(
        workspace["workspace"]["links"]["issue"],
        "https://github.com/team/project/issues/34"
    );
    assert_eq!(
        workspace["workspace"]["links"]["issue_title"],
        "Fix API timeout"
    );
}

/// A `curl` for `bin` that answers GitHub and Forgejo API requests from
/// files that [`script_api`] writes, logging each URL to `$HOME/curl-urls`.
pub(crate) fn install_api_curl(bin: &Path) {
    install_test_script(
        bin.join("curl"),
        r#"#!/bin/sh
cat >/dev/null
for arg; do url=$arg; done
printf '%s\n' "$url" >> "$HOME/curl-urls"
case "$url" in
  https://api.github.com/*) path=${url#https://api.github.com/};;
  *) path=${url#*/api/v1/};;
esac
key=$(printf %s "$path" | tr '/?&=' '____')
if [ -f "$HOME/api/$key" ]; then
  cat "$HOME/api/$key"
  printf '\n0 200'
else
  printf '{"message":"no fixture for %s"}\n0 404' "$key"
fi
"#,
    );
}

/// Answer `GET <endpoint>` under a forge's API root with `body` through the
/// fake curl.
pub(crate) fn script_api(home: &Path, endpoint: &str, body: &Value) {
    let key: String = endpoint
        .chars()
        .map(|c| if "/?&=".contains(c) { '_' } else { c })
        .collect();
    fs::create_dir_all(home.join("api")).unwrap();
    fs::write(home.join("api").join(key), body.to_string()).unwrap();
}

/// A Forgejo PR of `team/repo` from its `head` branch at commit `sha`.
pub(crate) fn forgejo_pull(number: u64, state: &str, head: &str, sha: &str) -> Value {
    let merged = state == "merged";
    serde_json::json!({"number": number, "title": format!("PR {number}"),
        "state": if merged { "closed" } else { state }, "merged": merged, "body": "", "labels": [],
        "head": {"ref": head, "sha": sha, "repo": {"full_name": "team/repo"}},
        "base": {"ref": "main"}, "mergeable": true})
}

pub(crate) fn wait_removed(fixture: &Fixture, name: &str) {
    try_wait_for(|| {
        (!fixture
            .ok(&["work"])
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["name"] == name))
        .then_some(())
    })
    .unwrap_or_else(|| {
        panic!(
            "PR cleanup did not remove workspace: {}",
            fixture.ok(&["work", "status", name])
        )
    });
}

fn wait_pr_error(fixture: &Fixture, name: &str, message: &str) {
    let mut inspection = Value::Null;
    try_wait_for(|| {
        inspection = fixture.ok(&["work", "status", name]);
        inspection["pr_cleanup"]["error"]
            .as_str()
            .is_some_and(|s| s.contains(message))
            .then_some(())
    })
    .unwrap_or_else(|| panic!("expected {message}: {inspection}"));
}

#[test]
fn merged_stops_agent_and_releases_resources_without_idle_delay() {
    let fixture = Fixture::with_config(Some("[cleanup.auto]\nenabled=false\n[resources.device]\n"));
    let workspace = fixture.add("merged");
    fixture.ok(&["acquire", "port", "web", "--workspace", "merged"]);
    fixture.ok(&["acquire", "resource", "device", "--workspace", "merged"]);
    let mut wrapper = fixture
        .command()
        .args(["run", "--workspace", "merged", "--"])
        .args(READABLE_SLEEP)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_registered_execution(&fixture, "merged");
    assert_eq!(fixture.acknowledge("merged")["type"], "ok");
    wait_removed(&fixture, "merged");
    assert!(!wrapper.wait().unwrap().success());
    assert!(!Path::new(workspace["path"].as_str().unwrap()).exists());
    assert!(
        fixture
            .ok(&["leases", "resource", "--all"])
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        fixture
            .ok(&["leases", "port", "--all"])
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn merged_retains_dirty_work_and_changed_head_across_restart_and_can_be_cancelled() {
    let mut fixture = Fixture::new();
    let workspace = fixture.add("retain");
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(path.join("dirty"), "keep").unwrap();
    assert_eq!(fixture.acknowledge("retain")["type"], "ok");
    wait_pr_error(&fixture, "retain", "uncommitted");
    git(path, &["add", "dirty"]);
    git(
        path,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-m",
            "new work",
        ],
    );
    fixture.restart();
    wait_pr_error(&fixture, "retain", "HEAD changed");
    assert!(path.join("dirty").exists());
    fixture.ok(&["unlink", "pr", "assigned", "--space", "retain"]);
    assert!(fixture.ok(&["work", "status", "retain"])["pr_cleanup"].is_null());
}

#[test]
fn pr_cleanup_can_be_disabled_independently() {
    let fixture = Fixture::with_config(Some("[cleanup.pr]\nenabled=false\n"));
    fixture.add("keep");
    // Linking does not depend on PR cleanup.
    fixture.add_github_origin();
    fixture.script_github(&github_pr(7, "keep"));
    fixture.ok(&["link", "pr", "7", "assigned", "--space", "keep"]);
    assert_eq!(
        fixture.ok(&["work", "status", "keep"])["workspace"]["links"]["prs"],
        serde_json::json!(["https://github.com/team/project/pull/7"])
    );
    fixture.ok(&["unlink", "pr", "assigned", "--space", "keep"]);
    let output = fixture.acknowledge("keep");
    assert_eq!(output["type"], "error");
    assert!(fixture.ok(&["work", "status", "keep"])["pr_cleanup"].is_null());
}

#[test]
fn legacy_pr_registrations_survive_disabled_cleanup_and_clear_after_restart() {
    let mut fixture = Fixture::with_config(Some(
        "[cleanup.pr]\nenabled=false\n[cleanup.auto]\nenabled=false\n",
    ));
    for name in ["watch", "acknowledged"] {
        let workspace = fixture.add(name);
        let head = git(
            Path::new(workspace["path"].as_str().unwrap()),
            &["rev-parse", "HEAD"],
        );
        let record = if name == "watch" {
            serde_json::json!({"url": "https://forge.example/team/repo/pulls/7", "head": null, "error": "previous lookup failure"})
        } else {
            serde_json::json!({"url": null, "head": head.trim(), "error": null})
        };
        let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
        db.execute(
            "INSERT INTO pr_cleanup(space_id,record) VALUES (?1,?2)",
            rusqlite::params![workspace["id"].as_str().unwrap(), record.to_string()],
        )
        .unwrap();
        fixture.restart();
        assert_eq!(fixture.ok(&["work", "status", name])["pr_cleanup"], record);
        assert_eq!(fixture.acknowledge(name)["type"], "error");
        assert_eq!(fixture.ok(&["work", "status", name])["pr_cleanup"], record);
        // Clear remains allowed while disabled, including for a scoped caller.
        assert_eq!(
            fixture.ok(&[
                "run",
                "--workspace",
                name,
                "--",
                support::shoal_binary(),
                "--json",
                "unlink",
                "pr",
                "assigned",
            ]),
            serde_json::json!({"registered": false})
        );
        fixture.restart();
        assert!(fixture.ok(&["work", "status", name])["pr_cleanup"].is_null());
        let again = fixture.run(&["unlink", "pr", "assigned", "--space", name]);
        assert!(!again.status.success(), "{again:?}");
        assert!(Path::new(workspace["path"].as_str().unwrap()).exists());
    }
}

#[test]
fn invalid_persisted_pr_registration_retains_workspace_and_can_be_cleared() {
    let mut fixture = Fixture::with_config(Some("[cleanup.auto]\nenabled=false\n"));
    let workspace = fixture.add("invalid");
    let db = rusqlite::Connection::open(fixture.root.path().join("state/state.db")).unwrap();
    for record in [
        serde_json::json!({"url": null, "head": null, "error": null}),
        serde_json::json!({"url": "https://forge.example/team/repo/pulls/7", "head": "abc123", "error": null}),
    ] {
        db.execute(
            "INSERT INTO pr_cleanup(space_id,record) VALUES (?1,?2)",
            rusqlite::params![workspace["id"].as_str().unwrap(), record.to_string()],
        )
        .unwrap();
        fixture.restart();
        let output = fixture.run(&["work", "status", "invalid"]);
        assert!(!output.status.success());
        assert!(Path::new(workspace["path"].as_str().unwrap()).exists());
        fixture.ok(&["unlink", "pr", "assigned", "--space", "invalid"]);
        assert!(fixture.ok(&["work", "status", "invalid"])["pr_cleanup"].is_null());
    }
}

#[test]
fn repository_config_sets_pr_cleanup_over_the_global_default() {
    let fixture = Fixture::with_config(Some("[cleanup.pr]\nenabled=false\n"));
    fs::write(
        fixture.repo.join(".shoal.toml"),
        "[cleanup.pr]\nenabled=true\n",
    )
    .unwrap();
    git(&fixture.repo, &["add", ".shoal.toml"]);
    git(
        &fixture.repo,
        &[
            "-c",
            "user.name=Shoal Test",
            "-c",
            "user.email=shoal@example.invalid",
            "commit",
            "-q",
            "-m",
            "enable pr cleanup",
        ],
    );
    fixture.add("merged");
    assert_eq!(fixture.acknowledge("merged")["type"], "ok");
    wait_removed(&fixture, "merged");
    // The saved config is the top layer.
    let saved = fixture.root.path().join("saved.toml");
    fs::write(&saved, "[cleanup.pr]\nenabled=false\n").unwrap();
    fixture.save_local_config(fixture.repo.to_str().unwrap(), &saved);
    fixture.add("kept");
    let output = fixture.acknowledge("kept");
    assert_eq!(output["type"], "error");
    assert!(fixture.ok(&["work", "status", "kept"])["pr_cleanup"].is_null());
}

#[test]
fn pr_watch_checks_github_state_and_commit_and_survives_restart() {
    let mut fixture = Fixture::with_config(Some("[cleanup.auto]\nenabled=false\n"));
    let workspace = fixture.add("watch");
    let failed = fixture.run(&["link", "pr", "56", "assigned", "--space", "watch"]);
    assert!(!failed.status.success());
    assert!(fixture.ok(&["work", "status", "watch"])["pr_cleanup"].is_null());
    git(
        &fixture.repo,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/team/repo.git",
        ],
    );
    let failed = fixture.run(&[
        "link",
        "pr",
        "https://github.com/team/repo/pull/56",
        "assigned",
        "--space",
        "watch",
    ]);
    assert!(!failed.status.success());
    assert!(fixture.ok(&["work", "status", "watch"])["pr_cleanup"].is_null());
    assert!(
        !fixture
            .run(&[
                "link",
                "pr",
                "https://github.com/other/repo/pull/56",
                "assigned",
                "--space",
                "watch"
            ])
            .status
            .success()
    );
    let write_response = |fixture: &Fixture, state: &str, head: &str| {
        let pull = "repos/team/repo/pulls/56";
        let responses = serde_json::json!({
            format!("GET {pull}"): {"body": {"number": 56, "title": "Watch",
                "state": if state == "open" { "open" } else { "closed" },
                "merged": state == "merged", "labels": [],
                "head": {"ref": "watch"}, "base": {"ref": "main"}}},
            format!("GET {pull}/commits?per_page=100&page=1"): {"body": [{"sha": head}]},
        });
        fixture.script_github(responses.as_object().unwrap());
    };
    let head = git(
        Path::new(workspace["path"].as_str().unwrap()),
        &["rev-parse", "HEAD"],
    )
    .trim()
    .to_owned();
    write_response(&fixture, "open", &head);
    fixture.ok(&[
        "link",
        "pr",
        "https://github.com/team/repo/pull/56",
        "assigned",
        "--space",
        "watch",
    ]);
    fixture.ok(&["unlink", "pr", "assigned", "--space", "watch"]);
    let events = fixture
        .command()
        .args(["--json", "internal", "events"])
        .output()
        .unwrap();
    assert!(events.status.success());
    let events: Vec<Value> = String::from_utf8(events.stdout)
        .unwrap()
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()
        .unwrap();
    for kind in ["linked", "unlinked"] {
        let event = events
            .iter()
            .find(|event| event["kind"] == kind)
            .unwrap_or_else(|| panic!("missing {kind} event"));
        assert_eq!(event["link"]["kind"], "pr");
        assert_eq!(event["link"]["relation"], "assigned");
        assert_eq!(event["link"]["url"], "https://github.com/team/repo/pull/56");
    }
    fixture.ok(&["link", "pr", "56", "assigned", "--space", "watch"]);
    fixture.ok(&["unlink", "pr", "assigned", "--space", "watch"]);
    // Scoped callers can omit the workspace and register by number too.
    fixture.ok(&[
        "run",
        "--workspace",
        "watch",
        "--",
        support::shoal_binary(),
        "--json",
        "link",
        "pr",
        "56",
        "assigned",
    ]);
    fixture.restart();
    assert_eq!(
        fixture.ok(&["work", "status", "watch"])["pr_cleanup"]["url"],
        "https://github.com/team/repo/pull/56"
    );
    // Watched PRs are looked up once the assignment is done.
    fixture.ok(&["done", "watch"]);
    // A persisted number must not silently follow a changed origin.
    git(
        &fixture.repo,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/other/repo.git",
        ],
    );
    fixture.restart();
    wait_pr_error(&fixture, "watch", "different repository");
    git(
        &fixture.repo,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/team/repo.git",
        ],
    );
    write_response(&fixture, "merged", &"a".repeat(40));
    // Checking the default branch for HEAD's changes must not reach GitHub.
    git(&fixture.repo, &["config", "protocol.https.allow", "never"]);
    fixture.restart();
    wait_pr_error(&fixture, "watch", "do not contain HEAD");
    let malformed =
        serde_json::json!({"GET repos/team/repo/pulls/56": {"body": "malformed output"}});
    fixture.script_github(malformed.as_object().unwrap());
    fixture.restart();
    wait_pr_error(&fixture, "watch", "expected");
    write_response(&fixture, "closed", &head);
    fixture.restart();
    assert!(Path::new(workspace["path"].as_str().unwrap()).exists());
    write_response(&fixture, "merged", &head);
    fixture.restart();
    wait_removed(&fixture, "watch");
}

#[test]
fn pr_watch_checks_forgejo_merge_and_commits_through_the_api() {
    let fixture = Fixture::new();
    let workspace = fixture.add("forgejo-watch");
    git(
        &fixture.repo,
        &[
            "remote",
            "add",
            "origin",
            "https://forge.example/team/repo.git",
        ],
    );
    let home = fixture.root.path();
    install_api_curl(&home.join("bin"));
    let head = git(
        Path::new(workspace["path"].as_str().unwrap()),
        &["rev-parse", "HEAD"],
    );
    let head = head.trim();
    script_api(
        home,
        "repos/team/repo/pulls/56",
        &forgejo_pull(56, "merged", "forgejo-watch", head),
    );
    script_api(
        home,
        "repos/team/repo/pulls/56/commits?limit=50&page=1",
        &serde_json::json!([{"sha": head}]),
    );
    fixture.ok(&["link", "pr", "56", "assigned", "--space", "forgejo-watch"]);
    // A merge alone waits for the agent's explicit done.
    assert!(fixture.ok(&["work", "status", "forgejo-watch"])["completion"].is_null());
    fixture.ok(&["done", "forgejo-watch"]);
    wait_removed(&fixture, "forgejo-watch");
}

#[test]
fn pr_wait_wakes_for_failed_ci_reviews_and_conflicts_with_own_scope() {
    let mut fixture = Fixture::with_config(Some("[cleanup.auto]\nenabled=false\n"));
    let workspace = fixture.add("watch");
    fixture.add("other");
    fixture.add_github_origin();
    let mut responses = github_pr(7, "watch");
    let mut checks = serde_json::json!([
        {"name":"rust","status":"in_progress","conclusion":null,"details_url":"rust/1"},
        {"name":"review","status":"completed","conclusion":"success","details_url":"review/1"}
    ]);
    let (runs, answer) = github_check_runs(checks.clone());
    responses.insert(runs, answer);
    responses.insert(
        "GET repos/team/project/pulls/7/comments?per_page=100&page=1".into(),
        serde_json::json!({"body": [{"id":1,"body":"review finding","user":{"login":"bot"}}]}),
    );
    fixture.script_github(&responses);
    fixture.ok(&["link", "pr", "7", "assigned", "--space", "watch"]);
    let binary = support::shoal_binary();
    let first = fixture.ok(&[
        "run",
        "--workspace",
        "watch",
        "--",
        binary,
        "--json",
        "watch",
        "pr",
    ]);
    assert_eq!(first["timed_out"], false);
    let kinds: Vec<_> = first["updates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|update| update["kind"].as_str().unwrap())
        .collect();
    // The passing check waits for the one still running.
    assert_eq!(kinds, ["comment"]);
    assert_eq!(
        first["updates"][0]["message"],
        "1 new inline comment from bot; read them with shoal view https://github.com/team/project/pull/7"
    );
    assert_eq!(
        first["updates"][0]["url"],
        "https://github.com/team/project/pull/7"
    );
    let denied = fixture.run(&[
        "run",
        "--workspace",
        "watch",
        "--",
        binary,
        "watch",
        "pr",
        "--space",
        "other",
    ]);
    assert!(!denied.status.success());
    fixture.restart();
    // Completion being disabled does not disable activity waits.
    let path = Path::new(workspace["path"].as_str().unwrap());
    fs::write(path.join(".shoal.toml"), "[cleanup.pr]\nenabled=false\n").unwrap();
    checks[0]["status"] = serde_json::json!("completed");
    checks[0]["conclusion"] = serde_json::json!("failure");
    let (runs, answer) = github_check_runs(checks);
    responses.insert(runs, answer);
    responses["GET repos/team/project/pulls/7"]["body"]["mergeable"] = serde_json::json!(false);
    fixture.script_github(&responses);
    let result = fixture
        .command()
        .current_dir(path)
        .args(["--json", "watch", "pr"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let second: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(second["updates"].as_array().unwrap().len(), 2);
    assert_eq!(second["updates"][0]["message"], "review: success");
    // The check has no Actions job; tests/forge.rs covers the log it adds.
    assert_eq!(second["updates"][1]["message"], "rust: failure");
    // A conflict is reported once a second poll sees it at the same commits.
    let third = fixture.ok(&["watch", "pr", "--space", "watch"]);
    assert_eq!(third["updates"].as_array().unwrap().len(), 1, "{third}");
    assert_eq!(third["updates"][0]["kind"], "merge_conflict");
    assert!(fixture.ok(&["work", "status", "watch"])["completion"].is_null());
    fixture.ok(&["unlink", "pr", "assigned", "--space", "watch"]);
    let missing = fixture.run(&["watch", "pr", "--space", "watch"]);
    assert!(!missing.status.success());
}

#[test]
fn a_newer_pr_wait_supersedes_the_older_one_without_taking_its_updates() {
    let fixture = Fixture::with_config(Some("[cleanup.auto]\nenabled=false\n"));
    fixture.add("waits");
    fixture.add_github_origin();
    let mut responses = github_pr(7, "waits");
    responses.insert(
        "GET repos/team/project/issues/7/comments?per_page=100&page=1".into(),
        serde_json::json!({"body": [{"id":1,"node_id":"IC_1","body":"review"}]}),
    );
    fixture.script_github(&responses);
    // Lookups after linking block until released, so the first wait is still polling when
    // the second starts.
    install_test_script(
        fixture.root.path().join("bin/curl"),
        r#"#!/bin/sh
if [ -f "$HOME/block" ]; then
  touch "$HOME/polled"
  while test ! -f "$HOME/release"; do sleep 0.02; done
fi
exec "$HOME/forge.py" "$@"
"#,
    );
    fixture.ok(&["link", "pr", "7", "assigned", "--space", "waits"]);
    fs::write(fixture.root.path().join("block"), "").unwrap();
    let wait = || {
        fixture
            .command()
            .args(["--json", "watch", "pr", "--space", "waits"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    let mut older = wait();
    let polled = fixture.root.path().join("polled");
    wait_until("the first wait's lookup", || polled.exists());
    let newer = wait();
    wait_until("the superseded wait", || {
        older.try_wait().unwrap().is_some()
    });
    let output = older.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let superseded: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(superseded["superseded"], true);
    assert!(superseded["updates"].as_array().unwrap().is_empty());
    fs::write(fixture.root.path().join("release"), "").unwrap();
    let output = newer.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let received: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(received["superseded"], false);
    assert_eq!(received["updates"][0]["kind"], "comment");
}

#[test]
fn unified_items_link_filter_and_watch_explicit_targets_without_completion() {
    let mut fixture = Fixture::with_config(Some("[cleanup.auto]\nenabled=false\n"));
    fixture.add("items");
    fixture.add("other");
    fixture.add_github_origin();
    // Issue `number` with one comment, and PR 7 with another.
    let write_issue = |fixture: &Fixture, number: u64, body: &str| {
        let mut responses = github_pr(7, "items");
        let comments = |number: u64| {
            format!("GET repos/team/project/issues/{number}/comments?per_page=100&page=1")
        };
        let issue = serde_json::json!({
            format!("GET repos/team/project/issues/{number}"): {"body": {"number": number,
                "title": "Issue", "state": "open", "body": "", "labels": [],
                "pull_request": null}},
            comments(number): {"body": [{"id": 1, "node_id": "IC_1", "body": body}]},
            comments(7): {"body": [{"id": 2, "node_id": "IC_2", "body": "PR comment"}]},
        });
        responses.extend(issue.as_object().unwrap().clone());
        fixture.script_github(&responses);
    };
    write_issue(&fixture, 1, "first");
    fixture.ok(&["link", "pr", "7", "assigned", "--space", "items"]);
    fixture.ok(&[
        "link",
        "https://github.com/team/project/issues/1",
        "assigned",
        "--space",
        "items",
    ]);
    let binary = support::shoal_binary();
    let filtered = fixture.ok(&[
        "run",
        "--workspace",
        "items",
        "--",
        binary,
        "--json",
        "watch",
        "pr",
    ]);
    assert_eq!(filtered["updates"].as_array().unwrap().len(), 1);
    assert_eq!(
        filtered["updates"][0]["url"],
        "https://github.com/team/project/pull/7"
    );
    let issue = fixture.ok(&["watch", "issue", "--space", "items"]);
    assert_eq!(
        issue["updates"][0]["url"],
        "https://github.com/team/project/issues/1"
    );
    write_issue(&fixture, 1, "changed");
    let all = fixture.ok(&["watch", "--space", "items"]);
    assert_eq!(all["updates"].as_array().unwrap().len(), 1);
    fixture.restart();
    write_issue(&fixture, 1, "changed after restart");
    let direct = fixture.ok(&[
        "watch",
        "https://github.com/team/project/issues/1",
        "--space",
        "items",
    ]);
    assert_eq!(direct["updates"].as_array().unwrap().len(), 1);
    assert!(
        !fixture
            .run(&["unlink", "issue", "2", "assigned", "--space", "items"])
            .status
            .success()
    );
    assert!(
        !fixture
            .run(&[
                "run",
                "--workspace",
                "items",
                "--",
                binary,
                "watch",
                "--workspace",
                "other"
            ])
            .status
            .success()
    );
    fixture.ok(&[
        "run",
        "--workspace",
        "items",
        "--",
        binary,
        "--json",
        "unlink",
        "issue",
        "1",
        "assigned",
    ]);
    let inspection = fixture.ok(&["work", "status", "items"]);
    assert!(inspection["issue"].is_null());
    assert_eq!(
        inspection["pr_cleanup"]["url"],
        "https://github.com/team/project/pull/7"
    );
    write_issue(&fixture, 2, "unlinked issue");
    let explicit = fixture.ok(&["watch", "issue", "2", "--space", "items"]);
    assert_eq!(
        explicit["updates"][0]["url"],
        "https://github.com/team/project/issues/2"
    );
    let inspection = fixture.ok(&["work", "status", "items"]);
    assert!(inspection["issue"].is_null() && inspection["completion"].is_null());
    write_issue(&fixture, 1, "relink");
    fixture.ok(&["link", "issue", "1", "assigned", "--space", "items"]);
    fixture.ok(&["unlink", "assigned", "--space", "items"]);
    assert!(fixture.ok(&["work", "status", "items"])["issue"].is_null());
    assert!(fixture.ok(&["work", "status", "items"])["pr_cleanup"].is_null());
    // With nothing linked, a watch waits for branch conflicts alone.
    let items = PathBuf::from(
        fixture.ok(&["work", "status", "items"])["workspace"]["path"]
            .as_str()
            .unwrap(),
    );
    commit_file(&items, "conflict", "workspace\n");
    commit_file(&fixture.repo, "conflict", "main\n");
    let branch = fixture.ok(&["watch", "--space", "items"]);
    assert_eq!(branch["updates"][0]["kind"], "merge_conflict");
    assert_eq!(branch["updates"][0]["url"], "items");
    assert_eq!(
        branch["updates"][0]["message"],
        "Conflicts with main: conflict"
    );
    let mut fork = github_pr(7, "fork-topic");
    fork["GET repos/team/project/pulls/7"]["body"]["head"]["repo"]["full_name"] =
        serde_json::json!("fork/project");
    fork.insert(
        "GET repos/team/project/issues/7/comments?per_page=100&page=1".into(),
        serde_json::json!({"body": [{"id": 2, "node_id": "IC_2", "body": "PR comment"}]}),
    );
    fixture.script_github(&fork);
    let explicit = fixture.ok(&["watch", "pr", "7", "--space", "items"]);
    assert_eq!(explicit["updates"].as_array().unwrap().len(), 1);
    assert!(fixture.ok(&["work", "status", "items"])["pr_cleanup"].is_null());
    assert!(
        !fixture
            .run(&[
                "watch",
                "https://github.com/other/project/issues/2",
                "--space",
                "items"
            ])
            .status
            .success()
    );
}

#[test]
fn watch_explicit_forgejo_fork_pr_without_linking_or_checkout() {
    let fixture = Fixture::with_config(Some("[cleanup.auto]\nenabled=false\n"));
    fixture.add("items");
    git(
        &fixture.repo,
        &[
            "remote",
            "add",
            "origin",
            "https://forge.example/team/repo.git",
        ],
    );
    let home = fixture.root.path();
    install_api_curl(&home.join("bin"));
    let sha = "a".repeat(40);
    let mut pull = forgejo_pull(7, "open", "fork-topic", &sha);
    pull["head"]["repo"]["full_name"] = "other/repo".into();
    script_api(home, "repos/team/repo/pulls/7", &pull);
    script_api(
        home,
        "repos/team/repo/issues/7/comments?limit=50&page=1",
        &serde_json::json!([{"id": 1, "body": "please test forks"}]),
    );
    script_api(
        home,
        "repos/team/repo/pulls/7/reviews?limit=50&page=1",
        &serde_json::json!([]),
    );
    script_api(
        home,
        &format!("repos/team/repo/commits/{sha}/status?limit=50"),
        &serde_json::json!({"total_count": 1, "statuses": [{"context": "rust", "status": "success"}]}),
    );
    let watched = fixture.ok(&["watch", "pr", "7", "--space", "items"]);
    let updates = watched["updates"].as_array().unwrap();
    assert_eq!(updates.len(), 2);
    assert_eq!(updates[0]["kind"], "comment");
    assert_eq!(updates[1]["message"], "rust: success");
    assert_eq!(updates[0]["url"], "https://forge.example/team/repo/pulls/7");
    let inspection = fixture.ok(&["work", "status", "items"]);
    assert!(inspection["pr_cleanup"].is_null());
    assert!(inspection["completion"].is_null());
    assert_eq!(inspection["workspace"]["branch"], "items");
}

#[test]
fn watch_forgejo_issue_reports_comments_closure_and_reopening() {
    let fixture = Fixture::with_config(Some("[cleanup.auto]\nenabled=false\n"));
    fixture.add("issue-watch");
    git(
        &fixture.repo,
        &[
            "remote",
            "add",
            "origin",
            "https://forge.example/team/repo.git",
        ],
    );
    // The unfiltered watch checks branch conflicts against the default branch.
    git(
        &fixture.repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    let home = fixture.root.path();
    install_api_curl(&home.join("bin"));
    let state = |state: &str| {
        script_api(
            home,
            "repos/team/repo/issues/8",
            &serde_json::json!({"number": 8, "title": "Issue", "state": state, "body": "",
                "labels": [], "pull_request": null}),
        )
    };
    state("open");
    script_api(
        home,
        "repos/team/repo/issues/8/comments?limit=50&page=1",
        &serde_json::json!([{"id": 1, "body": "first comment"}]),
    );
    fixture.ok(&["link", "issue", "8", "assigned", "--space", "issue-watch"]);
    let first = fixture.ok(&["watch", "issue", "--space", "issue-watch"]);
    assert_eq!(first["updates"][0]["kind"], "comment");
    state("closed");
    let closed = fixture.ok(&["watch", "--space", "issue-watch"]);
    assert_eq!(closed["updates"].as_array().unwrap().len(), 1);
    assert_eq!(closed["updates"][0]["kind"], "closed");
    state("open");
    let reopened = fixture.ok(&["watch", "issue", "8", "--space", "issue-watch"]);
    assert_eq!(reopened["updates"][0]["kind"], "reopened");
    assert!(fixture.ok(&["work", "status", "issue-watch"])["completion"].is_null());
}

#[test]
fn pr_wait_reads_forgejo_ci_and_conflicts_from_the_api() {
    let fixture = Fixture::with_config(Some("[cleanup.auto]\nenabled=false\n"));
    fixture.add("api");
    git(
        &fixture.repo,
        &[
            "remote",
            "add",
            "origin",
            "https://forge.example/team/repo.git",
        ],
    );
    let home = fixture.root.path();
    install_api_curl(&home.join("bin"));
    let sha = "2".repeat(40);
    let mut pull = forgejo_pull(7, "open", "api", &sha);
    pull["mergeable"] = false.into();
    script_api(home, "repos/team/repo/pulls/7", &pull);
    script_api(
        home,
        "repos/team/repo/issues/7/comments?limit=50&page=1",
        &serde_json::json!([]),
    );
    script_api(
        home,
        "repos/team/repo/pulls/7/reviews?limit=50&page=1",
        &serde_json::json!([]),
    );
    script_api(
        home,
        &format!("repos/team/repo/commits/{sha}/status?limit=50"),
        &serde_json::json!({"total_count": 2, "statuses": [
            {"context": "rust", "status": "failure"}, {"context": "gate", "status": "pending"}
        ]}),
    );
    fixture.ok(&["link", "pr", "7", "assigned", "--space", "api"]);
    let watched = fixture.ok(&["watch", "pr", "--space", "api"]);
    let updates = watched["updates"].as_array().unwrap();
    assert_eq!(
        updates
            .iter()
            .map(|update| update["kind"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["ci_completed"],
        "{watched}"
    );
    assert!(
        updates[0]["message"]
            .as_str()
            .unwrap()
            .starts_with("rust: failure; log unavailable:")
    );
    // Forgejo also shows a PR it is still checking as not mergeable, so the
    // conflict waits for a second poll.
    let confirmed = fixture.ok(&["watch", "pr", "--space", "api"]);
    assert_eq!(
        confirmed["updates"].as_array().unwrap().len(),
        1,
        "{confirmed}"
    );
    assert_eq!(confirmed["updates"][0]["message"], "PR has merge conflicts");
    assert!(
        fs::read_to_string(home.join("curl-urls"))
            .unwrap()
            .starts_with("https://forge.example/api/v1/repos/team/repo/pulls/7\n")
    );
}

#[test]
fn pr_wait_reports_forgejo_reviews_when_the_ci_lookup_fails() {
    let fixture = Fixture::with_config(Some("[cleanup.auto]\nenabled=false\n"));
    fixture.add("watch");
    git(
        &fixture.repo,
        &[
            "remote",
            "add",
            "origin",
            "https://forge.example/team/repo.git",
        ],
    );
    let home = fixture.root.path();
    install_api_curl(&home.join("bin"));
    let sha = "1".repeat(40);
    let status = format!("repos/team/repo/commits/{sha}/status?limit=50");
    let checks = |rust: &str| {
        serde_json::json!({"total_count": 2, "statuses": [
            {"context": "review/default", "status": "success"}, {"context": "rust", "status": rust}
        ]})
    };
    let review = |body: &str| serde_json::json!([{"id": 1, "state": "COMMENT", "body": body, "user": {"login": "review-bot"}}]);
    let reviews = "repos/team/repo/pulls/7/reviews?limit=50&page=1";
    script_api(
        home,
        "repos/team/repo/pulls/7",
        &forgejo_pull(7, "open", "watch", &sha),
    );
    script_api(
        home,
        "repos/team/repo/issues/7/comments?limit=50&page=1",
        &serde_json::json!([]),
    );
    script_api(home, &status, &checks("pending"));
    script_api(home, reviews, &review("A finding"));
    fixture.ok(&["link", "pr", "7", "assigned", "--space", "watch"]);
    let first = fixture.ok(&["watch", "pr", "--space", "watch"]);
    // The passing check waits for rust.
    assert_eq!(first["updates"].as_array().unwrap().len(), 1, "{first}");
    assert_eq!(first["updates"][0]["kind"], "comment");
    // A failed CI lookup reports itself without hiding a new review.
    fs::remove_file(
        home.join("api")
            .join(status.replace(['/', '?', '&', '='], "_")),
    )
    .unwrap();
    script_api(home, reviews, &review("Another finding"));
    let second = fixture.ok(&["watch", "pr", "--space", "watch"]);
    assert_eq!(second["updates"][0]["kind"], "comment", "{second}");
    assert_eq!(second["updates"][1]["kind"], "lookup_failed");
    let mut conflicting = forgejo_pull(7, "open", "watch", &sha);
    conflicting["mergeable"] = false.into();
    script_api(home, "repos/team/repo/pulls/7", &conflicting);
    script_api(home, &status, &checks("failure"));
    let third = fixture.ok(&["watch", "pr", "--space", "watch"]);
    assert_eq!(third["updates"].as_array().unwrap().len(), 2, "{third}");
    assert_eq!(third["updates"][0]["message"], "review/default: success");
    assert!(
        third["updates"][1]["message"]
            .as_str()
            .unwrap()
            .starts_with("rust: failure; log unavailable:")
    );
    let fourth = fixture.ok(&["watch", "pr", "--space", "watch"]);
    assert_eq!(fourth["updates"].as_array().unwrap().len(), 1, "{fourth}");
    assert_eq!(fourth["updates"][0]["kind"], "merge_conflict");
}

#[test]
fn merged_rechecks_head_after_removal_hooks() {
    for key in ["hooks.pre_remove", "hooks.pre_resource_release"] {
        let fixture = Fixture::new();
        fs::write(
            fixture.repo.join(".shoal.toml"),
            format!("{key} = 'hook.sh'\n[resources.signing]\n"),
        )
        .unwrap();
        install_test_script(
            fixture.repo.join("hook.sh"),
            "#!/bin/sh\ngit -c user.name=Test -c user.email=test@example.invalid commit --allow-empty -m 'hook work'\n",
        );
        git(&fixture.repo, &["add", "."]);
        git(
            &fixture.repo,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-m",
                "hook",
            ],
        );
        let workspace = fixture.add("hook");
        if key == "hooks.pre_resource_release" {
            fixture.ok(&["acquire", "resource", "signing", "--workspace", "hook"]);
        }
        assert_eq!(fixture.acknowledge("hook")["type"], "ok");
        wait_pr_error(&fixture, "hook", "HEAD changed during removal hooks");
        assert!(Path::new(workspace["path"].as_str().unwrap()).exists());
    }
}

#[test]
fn manual_removal_keeps_a_branch_its_hooks_committed_to() {
    for key in ["hooks.pre_remove", "hooks.pre_resource_release"] {
        let fixture = Fixture::new();
        fs::write(
            fixture.repo.join(".shoal.toml"),
            format!("{key} = 'hook.sh'\n[resources.signing]\n"),
        )
        .unwrap();
        install_test_script(
            fixture.repo.join("hook.sh"),
            concat!(
                "#!/bin/sh\n",
                "echo work > hook-work && git add hook-work\n",
                "git -c user.name=Test -c user.email=test@example.invalid ",
                "commit -m 'hook work'\n",
            ),
        );
        git(&fixture.repo, &["add", "."]);
        git(
            &fixture.repo,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-m",
                "hook",
            ],
        );
        let workspace = fixture.add("hook");
        if key == "hooks.pre_resource_release" {
            fixture.ok(&["acquire", "resource", "signing", "--workspace", "hook"]);
        }
        // The final inspection sees the hook's commit, which only the branch has.
        let removal = fixture.ok(&["rm", "hook", "--yes"]);
        assert_eq!(removal["branch_deleted"], false);
        assert!(!Path::new(workspace["path"].as_str().unwrap()).exists());
        let branch = workspace["branch"].as_str().unwrap();
        assert_eq!(
            git(&fixture.repo, &["log", "-1", "--format=%s", branch]).trim(),
            "hook work"
        );
    }
}

#[test]
fn invalid_item_actions_fail_before_a_picker_asks_for_their_space() {
    let fixture = Fixture::new();
    fixture.add("worker");
    let picked = fixture.root.path().join("picked");
    install_test_script(
        fixture.root.path().join("bin/fzf"),
        format!("#!/bin/sh\ntouch '{}'\n", picked.display()),
    );
    for args in [
        &["issue", "comment", "--body", " "][..],
        &["pr", "comment", "--body", " "][..],
    ] {
        let mut command = fixture.command();
        command.current_dir(fixture.root.path()).args(args);
        let (output, transcript) = fixture.interactive_command(&mut command, "");
        assert!(!output.status.success(), "{args:?}: {transcript}");
        assert!(!picked.exists(), "{args:?} opened a picker");
    }
}

#[test]
fn merge_asks_before_merging_files_changed_on_both_sides() {
    let fixture = Fixture::new();
    commit_file(
        &fixture.repo,
        ".shoal.toml",
        "[conflicts]\nsensitive = ['version']\n",
    );
    let worker = fixture.add("worker");
    let path = Path::new(worker["path"].as_str().unwrap());
    commit_file(path, "version", "2\n");
    commit_file(path, "feature", "new\n");
    commit_file(&fixture.repo, "other", "main\n");
    commit_file(&fixture.repo, "version", "2\n");
    let before = git(&fixture.repo, &["rev-parse", "main"]);

    let check = fixture.run(&["--json", "conflicts", "--workspace", "worker"]);
    assert_eq!(check.status.code(), Some(1), "{check:?}");
    let check: serde_json::Value = serde_json::from_slice(&check.stdout)
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&check.stderr)));
    assert_eq!(check["conflicts"], false);
    assert_eq!(
        check["changed_on_both_sides"],
        serde_json::json!(["version"])
    );
    let status = fixture.ok(&["work", "status", "worker"]);
    assert_eq!(
        status["changed_on_both_sides"]["files"],
        serde_json::json!(["version"])
    );

    let refused = fixture.run(&["merge", "worker"]);
    assert!(!refused.status.success());
    let (declined, transcript) = fixture.interactive(&["merge", "worker"], "n\n");
    assert!(!declined.status.success(), "{transcript}");
    assert_eq!(git(&fixture.repo, &["rev-parse", "main"]), before);
    // An agent's --yes leaves the question to whoever approves the merge.
    let binary = support::shoal_binary();
    let scoped = [
        "run",
        "--workspace",
        "worker",
        "--",
        binary,
        "--json",
        "merge",
        "--yes",
    ];
    let pending = fixture.run(&scoped);
    assert_eq!(pending.status.code(), Some(2));
    let pending: serde_json::Value = serde_json::from_slice(&pending.stdout).unwrap();
    // The approver is told which sensitive files changed on both sides.
    let reason = pending["request"]["reason"].as_str().unwrap();
    assert!(reason.ends_with(": version."), "{reason}");

    // Merging makes a merge commit in the checkout, which needs an identity.
    git(&fixture.repo, &["config", "user.name", "Test"]);
    git(
        &fixture.repo,
        &["config", "user.email", "test@example.invalid"],
    );
    assert_eq!(fixture.ok(&["merge", "--yes", "worker"])["updated"], true);
    assert_eq!(
        fs::read_to_string(fixture.repo.join("feature")).unwrap(),
        "new\n"
    );
}

#[test]
fn view_shows_linked_items_with_status_and_discussion() {
    let fixture = Fixture::with_config(Some("[cleanup.auto]\nenabled=false\n"));
    fixture.add("view");
    fixture.add_github_origin();
    let bin = fixture.root.path().join("bin");
    let root = fixture.root.path();
    // Longer than a protocol frame: lookups run in the CLI.
    let body = "Steps ".repeat(20_000);
    let review = serde_json::json!({"id":11,"node_id":"PRR_11","user":{"login":"bot"},
        "state":"CHANGES_REQUESTED","body":"One blocker","submitted_at":"t3"});
    let inline = serde_json::json!({"id":21,"pull_request_review_id":11,"user":{"login":"bot"},
        "path":"src/a.rs","line":3,"body":"Handle None","created_at":"t3"});
    let mut responses = github_pr(7, "view");
    let (runs, answer) = github_check_runs(serde_json::json!([
        {"name":"rust","status":"completed","conclusion":"failure","details_url":"rust/1"}
    ]));
    responses.insert(runs, answer);
    let pull = &mut responses["GET repos/team/project/pulls/7"]["body"];
    pull["mergeable"] = serde_json::json!(false);
    pull["title"] = serde_json::json!("Fix crash");
    pull["auto_merge"] = serde_json::json!({"merge_method": "rebase"});
    let pages = "?per_page=100&page=1";
    let views = serde_json::json!({
        "GET repos/team/project/issues/3": {"body": {"number":3,"title":"Crash","body":body,
            "state":"open","user":{"login":"ann"},"labels":[{"name":"bug"}],"created_at":"t0"}},
        format!("GET repos/team/project/issues/3/comments{pages}"): {"body": [{"id":31,
            "user":{"login":"bob"},"created_at":"t1","body":"Same here"}]},
        // The forge answers for another issue than the one asked for.
        "GET repos/team/project/issues/9": {"body": {"number":3}},
        format!("GET repos/team/project/issues/9/comments{pages}"): {"body": []},
        format!("GET repos/team/project/pulls/7/reviews{pages}"): {"body": [review]},
        format!("GET repos/team/project/pulls/7/comments{pages}"): {"body": [inline]},
        "POST graphql": {"body": {"data":{"repository":{"pullRequest":{"reviewThreads":{
            "pageInfo":{"hasNextPage":false,"endCursor":null},
            "nodes":[{"id":"T_1","isResolved":false,"comments":{
                "pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[{"databaseId":21}]}}]}}}}}},
        "GET user": {"body": {"login":"me"}},
        format!("GET repos/team/project/issues/3/reactions{pages}"):
            {"body": [{"id":1,"content":"heart","user":{"login":"me"}}]},
        format!("GET repos/team/project/issues/7/reactions{pages}"): {"body": []},
        format!("GET repos/team/project/issues/comments/31/reactions{pages}"): {"body": []},
        format!("GET repos/team/project/pulls/comments/21/reactions{pages}"): {"body": []},
    });
    responses.extend(views.as_object().unwrap().clone());
    fixture.script_github(&responses);
    fixture.ok(&["link", "issue", "3", "assigned", "--space", "view"]);
    fixture.ok(&["link", "pr", "7", "assigned", "--space", "view"]);

    let views = fixture.ok(&["view", "--space", "view"]);
    let views = views.as_array().unwrap();
    assert_eq!(views.len(), 2);
    let issue = views.iter().find(|view| view["kind"] == "issue").unwrap();
    assert_eq!(issue["url"], "https://github.com/team/project/issues/3");
    assert_eq!(issue["state"], "open");
    assert_eq!(issue["body"], body);
    assert_eq!(issue["comments"][0]["body"], "Same here");
    assert_eq!(
        issue["reactions"],
        serde_json::json!([{"content": "heart", "count": 1, "reacted": true}])
    );
    let pr = views.iter().find(|view| view["kind"] == "pr").unwrap();
    assert_eq!(pr["pr"]["merge_conflicts"], true);
    assert_eq!(pr["pr"]["checks"][0]["result"], "failure");
    assert_eq!(pr["pr"]["review"], "changes_requested");
    assert_eq!(
        pr["pr"]["auto_merge"],
        serde_json::json!({"method": "rebase"})
    );
    let finding = &pr["reviews"][0]["comments"][0];
    assert_eq!(finding["path"], "src/a.rs");
    assert_eq!(
        (&finding["id"], &finding["thread"], &finding["resolved"]),
        (
            &serde_json::json!(21),
            &serde_json::json!(21),
            &serde_json::json!(false)
        )
    );
    // This forge answers no log, comparison or run lookup; tests/forge.rs
    // covers failed logs, how far a PR is behind and runs waiting for
    // approval.
    let errors = pr["errors"].as_array().unwrap();
    assert!(
        errors.iter().all(|error| {
            let error = error.as_str().unwrap();
            error.starts_with("rust: log unavailable: ")
                || error.starts_with("behind lookup failed: ")
                || error.starts_with("waiting CI runs lookup failed: ")
        }),
        "{pr}"
    );

    let status = fixture.ok(&["work", "status", "view"]);
    assert_eq!(status["issue_status"]["state"], "open");

    let brief = fixture.ok(&["view", "pr", "--no-comments", "--space", "view"]);
    assert_eq!(brief.as_array().unwrap().len(), 1);
    assert!(brief[0].get("reviews").is_none());

    let text = fixture.run(&["view", "pr", "7", "--space", "view"]);
    assert!(text.status.success());
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(text.contains("PR #7: Fix crash"), "{text}");
    assert!(text.contains("Branch:   view → main"), "{text}");
    assert!(text.contains(", auto-merge (rebase)"), "{text}");
    assert!(
        text.contains("src/a.rs:3 · bot · t3 · comment 21 · unresolved"),
        "{text}"
    );

    // Ctrl-V in the picker views one of the workspace's items, chosen
    // among them with Enter, or with $VIEW_KEY.
    install_test_script(
        bin.join("fzf"),
        r#"#!/bin/sh
cat > "$HOME/picker-input"
case "$*" in
*'View> '*)
  cp "$HOME/picker-input" "$HOME/view-rows"
  printf '%s\n' "$VIEW_KEY"
  grep /pull/ "$HOME/picker-input" ;;
*) echo ctrl-v; head -n 1 "$HOME/picker-input" ;;
esac
"#,
    );
    let (output, transcript) = fixture.terminal_command(fixture.command().arg("work"), "", true);
    assert!(output.status.success(), "{transcript}");
    let rows = fs::read_to_string(root.join("view-rows")).unwrap();
    assert_eq!(rows.lines().count(), 2, "{rows}");
    assert!(
        rows.contains("Issue") && rows.contains("#3  Crash") && rows.contains("#7"),
        "{rows}"
    );
    assert!(transcript.contains("PR #7: Fix crash"), "{transcript}");
    assert!(!transcript.contains("Same here"), "{transcript}");

    // `--web` opens the selected items in the browser and prints each URL,
    // without waiting for an opener that runs the browser until it closes.
    let opened = root.join("opened");
    let release = root.join("release");
    assert!(
        Command::new("mkfifo")
            .arg(&release)
            .status()
            .unwrap()
            .success()
    );
    install_test_script(
        bin.join(if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        }),
        r#"#!/bin/sh
printf '%s\n' "$1" >> "$HOME/opened"
cat "$HOME/release" > /dev/null
"#,
    );
    let opened_lines = || {
        fs::read_to_string(&opened)
            .map(|text| text.lines().count())
            .unwrap_or(0)
    };
    let mut web = fixture.command();
    web.args(["view", "issue", "3", "--web", "--space", "view"]);
    let (output, transcript) = fixture.terminal_command(&mut web, "", true);
    assert!(output.status.success(), "{transcript}");
    let issue = "https://github.com/team/project/issues/3";
    assert!(transcript.contains(issue), "{transcript}");
    wait_until("the opener's first URL", || opened_lines() == 1);
    fs::write(&release, "").unwrap();
    let mut web = fixture.command();
    web.args(["--json", "view", "pr", "7", "--web", "--space", "view"]);
    let (output, transcript) = fixture.terminal_command(&mut web, "", false);
    assert!(output.status.success(), "{transcript}");
    let pr = "https://github.com/team/project/pull/7";
    let web: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(web, serde_json::json!([pr]));
    wait_until("the opener's second URL", || opened_lines() == 2);
    fs::write(&release, "").unwrap();
    // Ctrl-O in the chooser opens the chosen item instead.
    let (output, transcript) = fixture.terminal_command(
        fixture.command().arg("work").env("VIEW_KEY", "ctrl-o"),
        "",
        true,
    );
    assert!(output.status.success(), "{transcript}");
    assert!(!transcript.contains("PR #7: Fix crash"), "{transcript}");
    wait_until("the chooser's URL", || opened_lines() == 3);
    fs::write(&release, "").unwrap();
    assert_eq!(
        fs::read_to_string(&opened).unwrap(),
        format!("{issue}\n{pr}\n{pr}\n")
    );
    // Without an opener the URL is still printed, and the command fails.
    let nothing = root.join("no-programs");
    fs::create_dir(&nothing).unwrap();
    let missing = fixture
        .command()
        .env("PATH", &nothing)
        .args(["view", "issue", "3", "--web", "--space", "view"])
        .output()
        .unwrap();
    assert!(!missing.status.success(), "{missing:?}");
    assert!(
        String::from_utf8_lossy(&missing.stdout).contains(issue),
        "{missing:?}"
    );
    // A workspace process cannot open windows on the user's screen.
    let scoped = fixture.run(&[
        "run",
        "--workspace",
        "view",
        "--",
        support::shoal_binary(),
        "view",
        "--web",
    ]);
    assert!(!scoped.status.success());

    // An unreadable item reports its error and fails the command.
    let missing = fixture.run(&["--json", "view", "issue", "9", "--space", "view"]);
    assert_eq!(missing.status.code(), Some(1));
    let missing: Value = serde_json::from_slice(&missing.stdout).unwrap();
    assert!(
        !missing[0]["errors"].as_array().unwrap().is_empty(),
        "{missing}"
    );
}

#[test]
fn merged_base_prs_retarget_and_restack_the_workspaces_stacked_on_them_on_github() {
    restack_on_merge(true, &[(1, "lower", "main")], "lower");
}

#[test]
fn merged_base_prs_retarget_and_restack_the_workspaces_stacked_on_them_on_forgejo() {
    restack_on_merge(false, &[(1, "lower", "main")], "lower");
}

#[test]
fn a_base_pr_from_another_branch_retargets_the_prs_stacked_on_it() {
    restack_on_merge(false, &[(1, "lower-stacked", "main")], "lower-stacked");
}

#[test]
fn chained_base_prs_retarget_the_prs_stacked_on_them_past_every_merged_branch() {
    let chain = [(1, "lower", "main"), (3, "lower-2", "lower")];
    restack_on_merge(false, &chain, "lower-2");
}

/// Merge the PRs of the `lower` workspace, each a number with its head and
/// base branch, and check that `upper`, whose PR targets `upper_base`, is
/// restacked and its PR retargeted to `main`.
fn restack_on_merge(github: bool, lower_prs: &[(u64, &str, &str)], upper_base: &str) {
    // Both forges read and PATCH the PR through their API with the token.
    let mut fixture = Fixture::with_config(Some(
        "machine.forges.\"github.com\".token = \"secret123\"\n\
             machine.forges.\"forge.example\".token = \"secret123\"\n\
             [cleanup.auto]\nenabled=false\n",
    ));
    let lower = fixture.add("lower");
    let repo = fixture.repo.to_str().unwrap().to_owned();
    fixture.ok(&["start", &repo, "upper", "--base", "lower"]);
    let origin = if github {
        "git@github.com:team/project.git"
    } else {
        "https://forge.example/team/project.git"
    };
    git(&fixture.repo, &["remote", "add", "origin", origin]);
    git(
        &fixture.repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    let root = fixture.root.path().to_owned();
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    install_api_curl(&bin);
    let fake_api = fs::read_to_string(bin.join("curl")).unwrap();
    install_test_script(
        bin.join("curl"),
        fake_api.replacen(
            "cat >/dev/null\n",
            r#"case "$*" in *PATCH*)
  printf '%s\n' "$@" > "$HOME/curl-args"; cat > "$HOME/curl-stdin"
  echo "2 main" >> "$HOME/edits"; printf '{}\n0 200'; exit;;
esac
cat >/dev/null
"#,
            1,
        ),
    );
    let head = git(
        Path::new(lower["path"].as_str().unwrap()),
        &["rev-parse", "HEAD"],
    );
    let head = head.trim();
    for (number, _, _) in lower_prs {
        let commits = if github {
            format!("repos/team/project/pulls/{number}/commits?per_page=100&page=1")
        } else {
            format!("repos/team/project/pulls/{number}/commits?limit=50&page=1")
        };
        script_api(&root, &commits, &serde_json::json!([{"sha": head}]));
    }
    let write_pr = |number: u64, state: &str, head_branch: &str, base: &str| {
        let mut rest = forgejo_pull(number, &state.to_lowercase(), head_branch, head);
        rest["base"]["ref"] = base.into();
        rest["head"]["repo"]["full_name"] = "team/project".into();
        script_api(&root, &format!("repos/team/project/pulls/{number}"), &rest);
    };
    for &(number, head_branch, base) in lower_prs {
        write_pr(number, "Open", head_branch, base);
        let number = number.to_string();
        fixture.ok(&["link", "pr", &number, "assigned", "--space", "lower"]);
    }
    write_pr(2, "Open", "upper", upper_base);
    fixture.ok(&["link", "pr", "2", "assigned", "--space", "upper"]);
    fixture.restart();
    assert_eq!(
        fixture.ok(&["work", "status", "upper"])["workspace"]["base_workspace"]["name"],
        "lower"
    );

    for &(number, head_branch, base) in lower_prs {
        write_pr(number, "Merged", head_branch, base);
    }
    fixture.restart();
    wait_until("restacked upper", || {
        fixture.ok(&["work", "status", "upper"])["workspace"]["base_workspace"].is_null()
    });
    let edits = fs::read_to_string(root.join("edits")).unwrap();
    assert_eq!(edits, "2 main\n");
    let (api, authorization) = if github {
        ("https://api.github.com", "Bearer secret123")
    } else {
        ("https://forge.example/api/v1", "token secret123")
    };
    let args = fs::read_to_string(root.join("curl-args")).unwrap();
    assert!(args.contains("PATCH\n"), "{args}");
    assert!(args.contains("{\"base\":\"main\"}\n"), "{args}");
    assert!(
        args.contains(&format!("{api}/repos/team/project/pulls/2\n")),
        "{args}"
    );
    assert!(!args.contains("secret123"), "{args}");
    let config = fs::read_to_string(root.join("curl-stdin")).unwrap();
    assert_eq!(
        config,
        format!("header = \"Authorization: {authorization}\"\n")
    );
    // A PR activity lookup that failed around the restart may report
    // before the update the restack queued.
    let updates = fixture.ok(&["watch", "pr", "--space", "upper", "--timeout", "1"]);
    let update = updates["updates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|update| update["kind"] == "base_merged")
        .unwrap_or_else(|| panic!("{updates}"));
    let message = update["message"].as_str().unwrap();
    assert!(message.contains(head), "{message}");
    let events = fixture
        .command()
        .args(["--json", "internal", "events"])
        .output()
        .unwrap();
    let restacked = String::from_utf8(events.stdout)
        .unwrap()
        .lines()
        .any(|line| {
            let event: Value = serde_json::from_str(line).unwrap();
            event["kind"] == "base_changed" && event["name"] == "upper" && event["cause"] == "pr"
        });
    assert!(restacked);
}

#[test]
fn failed_restacks_are_reported_even_when_pr_cleanup_is_disabled() {
    let mut fixture = Fixture::with_config(Some("[cleanup.auto]\nenabled=false\n"));
    let lower = fixture.add("lower");
    let repo = fixture.repo.to_str().unwrap().to_owned();
    fixture.ok(&["start", &repo, "upper", "--base", "lower"]);
    fixture.add_github_origin();
    let root = fixture.root.path().to_owned();
    // Fail the requests whose URL contains the pattern in ~/fail.
    install_test_script(
        root.join("bin/curl"),
        "#!/bin/sh\nfor arg; do url=$arg; done\n\
         [ -e \"$HOME/fail\" ] && case \"$url\" in *\"$(cat \"$HOME/fail\")\"*) echo failed >&2; exit 7;; esac\n\
         exec \"$HOME/forge.py\" \"$@\"\n",
    );
    let head = git(
        Path::new(lower["path"].as_str().unwrap()),
        &["rev-parse", "HEAD"],
    );
    let prs = std::cell::RefCell::new(serde_json::Map::new());
    let write_pr =
        |fixture: &Fixture, number: u64, state: &str, branch: &str, base: &str, commit: &str| {
            let pull = format!("repos/team/project/pulls/{number}");
            let mut prs = prs.borrow_mut();
            prs.insert(
                format!("GET {pull}"),
                serde_json::json!({"body": {"number": number, "title": "Stacked",
                "state": if state == "open" { "open" } else { "closed" },
                "merged": state == "merged", "labels": [],
                "head": {"ref": branch, "repo": {"full_name": "team/project"}},
                "base": {"ref": base}}}),
            );
            prs.insert(
                format!("GET {pull}/commits?per_page=100&page=1"),
                serde_json::json!({"body": [{"sha": commit}]}),
            );
            prs.insert(format!("PATCH {pull}"), serde_json::json!({"body": {}}));
            fixture.script_github(&prs);
        };
    write_pr(&fixture, 1, "open", "lower", "main", head.trim());
    write_pr(&fixture, 2, "open", "upper", "lower", head.trim());
    fixture.ok(&["link", "pr", "1", "assigned", "--space", "lower"]);
    fixture.ok(&["link", "pr", "2", "assigned", "--space", "upper"]);
    fs::write(
        root.join(".config/shoal/config.toml"),
        "[cleanup.auto]\nenabled=false\n[cleanup.pr]\nenabled=false\n",
    )
    .unwrap();
    // An unknown merge holds the base back and records why, without alerts.
    write_pr(&fixture, 1, "merged", "lower", "main", head.trim());
    fs::write(root.join("fail"), "commits").unwrap();
    fixture.restart();
    wait_until("recorded merge lookup failure", || {
        fixture.ok(&["work", "status", "lower"])["pr_cleanup"]["error"]
            .as_str()
            .is_some_and(|error| error.contains("could not check whether"))
    });
    assert!(fixture.ok(&["alerts"]).as_array().unwrap().is_empty());
    // A merge that left out the base's HEAD keeps the stack, since rebasing
    // onto it would drop the missing commits.
    fs::remove_file(root.join("fail")).unwrap();
    write_pr(&fixture, 1, "merged", "lower", "main", &"0".repeat(40));
    fixture.restart();
    wait_until("recorded unmerged base HEAD", || {
        fixture.ok(&["work", "status", "lower"])["pr_cleanup"]["error"]
            .as_str()
            .is_some_and(|error| error.contains("do not contain the base workspace's HEAD"))
    });
    assert_eq!(
        fixture.ok(&["work", "status", "upper"])["workspace"]["base_workspace"]["name"],
        "lower"
    );
    // A failure after the merge is confirmed is reported: the merged PR now
    // has no base branch, so looking up the branch it merged into fails. A
    // failed retarget of the stacked PR would not do, since the restack only
    // reports it in the stacked workspace's update.
    write_pr(&fixture, 1, "merged", "lower", "", head.trim());
    fixture.restart();
    wait_until("reported restack failure", || {
        fixture.ok(&["work", "status", "lower"])["pr_cleanup"]["error"]
            .as_str()
            .is_some_and(|error| error.contains("has no head or base branch"))
    });
    assert_eq!(
        fixture.ok(&["work", "status", "upper"])["workspace"]["base_workspace"]["name"],
        "lower"
    );
    // Recovery restacks and clears the recorded error.
    write_pr(&fixture, 1, "merged", "lower", "main", head.trim());
    fixture.restart();
    wait_until("restack after recovery", || {
        let lower = fixture.ok(&["work", "status", "lower"]);
        lower["pr_cleanup"]["error"].is_null()
            && fixture.ok(&["work", "status", "upper"])["workspace"]["base_workspace"].is_null()
    });
}

#[test]
fn a_fork_prs_head_opens_in_one_workspace_at_a_commit() {
    // The number of the review `review run` queued, as its output names it.
    let review_number = |output: &Output| {
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        stdout
            .split_whitespace()
            .skip_while(|word| *word != "Review")
            .nth(1)
            .unwrap_or_else(|| panic!("{stdout}"))
            .to_owned()
    };
    let fixture = Fixture::with_config(Some(
        "agent.default = 'reviewer'\n[agents.reviewer]\ncommand = ['sh']\nreview = ['-c', 'exit 1', '{prompt}', '{schema}']\n[commands]\nreview = ['printf', '%s\\n', '{diff_base}']\n",
    ));
    let author = forge_origin(&fixture);
    let remote = fixture.root.path().join("origin.git");
    // A fork's head reaches the repository only under the PR's ref.
    let push_fork_head = |file: &str| {
        git(&author, &["switch", "-q", "--detach", "main"]);
        fs::write(author.join(file), "fork\n").unwrap();
        git(&author, &["add", file]);
        git(
            &author,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-qm",
                file,
            ],
        );
        let head = git(&author, &["rev-parse", "HEAD"]).trim().to_owned();
        git(&remote, &["fetch", "-q", author.to_str().unwrap(), &head]);
        git(&remote, &["update-ref", "refs/pull/8/head", &head]);
        head
    };
    let bin = fixture.root.path().join("bin");
    install_api_curl(&bin);
    let script = |head: &str| {
        let mut pull = forgejo_pull(8, "open", "topic", head);
        pull["head"]["repo"]["full_name"] = "someone/project".into();
        script_api(fixture.root.path(), "repos/team/project/pulls/8", &pull);
    };
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let url = "https://forge.example/team/project/pulls/8";
    let run = |args: &[&str]| {
        fixture
            .command()
            .args(args)
            .current_dir(&fixture.repo)
            .env("PATH", &path)
            .output()
            .unwrap()
    };
    let first = push_fork_head("first");
    script(&first);
    // An app's request for the head with an agent that can run is refused
    // before anything is created, since the head holds no agent of its own.
    let before = fixture.ok(&["work"]);
    let start = shoal_client::protocol::OperationMethod::StartWorkspace {
        repository: fixture.repo.to_str().unwrap().into(),
        start: shoal_client::protocol::operations::WorkspaceStart::PullHead {
            url: url.into(),
            head: shoal_client::protocol::PullHead {
                number: shoal_client::forge::numbers::PullNumber::new(8),
                reference: "refs/pull/8/head".into(),
                commit: first.clone(),
                base: None,
            },
        },
        base: None,
        path: None,
        git_profile: None,
        parent: None,
        options: Default::default(),
        issue: None,
        pr: Some(url.into()),
        agent: Some(Box::new(
            shoal_client::protocol::operations::QueuedLaunch::new("reviewer".into()),
        )),
        env: None,
    };
    let refused =
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(shoal_client::connection::request::<
                shoal_client::protocol::operations::StartedWorkspace,
            >(&fixture.connector(), start));
    assert!(refused.is_err(), "expected a refusal: {refused:?}");
    assert_eq!(fixture.ok(&["work"]), before);
    let output = run(&["review", "run", "--pr", "8", "--agent", "reviewer"]);
    assert!(output.status.success(), "{output:?}");
    let ended = fixture.ok(&["watch", "review", "1"]);
    assert_eq!(ended["head"], first, "{ended}");
    let workspace = &fixture.ok(&["work", "status", "pr-8"])["workspace"];
    assert_eq!(workspace["branch"], Value::Null);
    assert_eq!(workspace["at"]["made_for"], serde_json::json!({"pr": 8}));
    assert_eq!(workspace["setup_skipped"], true);
    // Its diff, and a manual review's {diff_base}, compare with the PR's base.
    assert_eq!(workspace["base_ref"], "refs/remotes/origin/main");
    assert_eq!(workspace["links"]["prs"], serde_json::json!([url]));

    // Adding the PR reuses its workspace; it holds no agent.
    let output = run(&["--json", "start", url]);
    assert!(output.status.success(), "{output:?}");
    let added: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(added["id"], workspace["id"]);
    let output = run(&["start", url, "--agent", "reviewer"]);
    assert!(!output.status.success());
    // On a terminal too, the PR settles what the addition opens.
    let (output, transcript) = fixture.interactive_command(
        fixture
            .command()
            .args(["start", url])
            .current_dir(&fixture.repo)
            .env("PATH", &path),
        "",
    );
    assert!(output.status.success(), "{output:?} {transcript}");
    assert!(!transcript.contains("Workspace>"), "{transcript}");
    let output = run(&["start", url, "--git-profile", "work"]);
    assert!(!output.status.success());

    // A moved head moves the workspace with it.
    let second = push_fork_head("second");
    script(&second);
    let output = run(&["review", "run", url, "--agent", "reviewer", "--no-post"]);
    assert!(output.status.success(), "{output:?}");
    fixture.ok(&["watch", "review", &review_number(&output)]);
    let moved = &fixture.ok(&["work", "status", "pr-8"])["workspace"];
    assert_eq!(moved["id"], workspace["id"]);
    assert_eq!(moved["at"]["commit"], second);
    assert_eq!(fixture.ok(&["work"]).as_array().unwrap().len(), 1);
    // A manual review compares the PR's head with its base.
    let output = run(&["review", "run", url, "--manual"]);
    assert!(output.status.success(), "{output:?}");
    let main = git(&remote, &["rev-parse", "main"]);
    assert_eq!(String::from_utf8_lossy(&output.stdout), main);

    // A review reads the PR's head, not what was committed on top of it.
    let moved_path = PathBuf::from(moved["path"].as_str().unwrap());
    fs::write(moved_path.join("local"), "local\n").unwrap();
    git(&moved_path, &["add", "local"]);
    git(
        &moved_path,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "local",
        ],
    );
    let output = run(&["review", "run", url, "--agent", "reviewer", "--no-post"]);
    assert!(output.status.success(), "{output:?}");
    let ended = fixture.ok(&["watch", "review", &review_number(&output)]);
    assert_eq!(ended["head"], second, "{ended}");
    // A manual review reads the checkout, so it refuses one past the PR's head.
    let output = run(&["review", "run", url, "--manual"]);
    assert!(!output.status.success());

    // Adding the PR runs its setup hooks once; reviewing it reopens the
    // workspace without running them again.
    fixture.ok(&["rm", "pr-8", "--yes"]);
    let local = fixture.root.path().join("local.toml");
    let hook = fixture.root.path().join("post-setup.sh");
    install_test_script(&hook, "#!/bin/sh\necho ran >> \"$HOME/post-setup-ran\"\n");
    fs::write(&local, format!("hooks.post_setup = '{}'\n", hook.display())).unwrap();
    fixture.save_local_config(fixture.repo.to_str().unwrap(), &local);
    let output = run(&["start", url]);
    assert!(output.status.success(), "{output:?}");
    let output = run(&["review", "run", url, "--agent", "reviewer", "--no-post"]);
    assert!(output.status.success(), "{output:?}");
    let ran = fs::read_to_string(fixture.root.path().join("post-setup-ran")).unwrap();
    assert_eq!(ran, "ran\n");
}
