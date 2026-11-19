use std::sync::atomic::Ordering;

use shoal_daemon_base::approval_requests::ApprovalRequestExt;
use shoal_protocol::approvals::{PushSpecification, Specification, Target};
use shoal_protocol::config::repo::Lifetime;

use super::*;
use crate::{
    daemon::Requests,
    shoal_dirs::{Contents, ShoalDir},
    stand_in::Fixture,
    test_support::{git, install_test_script},
};

/// A fake gate agent: it records each run as `new` or `resume <session>`,
/// the latest resumed run's prompt in `resumed`, its scope token and state
/// directory in `scope`, reports session `s<run>` or the one it resumes,
/// fails while `fail` exists, cannot resume while `stale` exists, waits
/// while `hold` exists, and returns `verdict.json`.
const AGENT: &str = r#"#!/bin/sh
dir='{dir}'
if [ "$1" = resume ]; then echo "resume $2" >> "$dir/runs"; session=$2; printf '%s' "$3" > "$dir/resumed"
else echo new >> "$dir/runs"; session="s$(wc -l < "$dir/runs" | tr -d ' ')"; fi
echo "session id: $session" >&2
printf '%s\n%s' "$SHOAL_SCOPE_TOKEN" "$SHOAL_STATE_DIR" > "$dir/scope.tmp" && mv "$dir/scope.tmp" "$dir/scope"
[ -e "$dir/fail" ] && exit 1
[ "$1" = resume ] && [ -e "$dir/stale" ] && exit 1
while [ -e "$dir/hold" ]; do sleep 0.1; done
cat "$dir/verdict.json"
"#;

/// A workspace whose pushes the gate decides, by the fake [`AGENT`], with
/// `a push to a tag` as the one case it passes on.
async fn fixture() -> Fixture {
    fixture_deciding_by("fake").await
}

/// [`fixture`] with `gate` as `[gate] agent`.
async fn fixture_deciding_by(gate: &str) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("gate.sh");
    let dir = root.path().to_string_lossy();
    install_test_script(&script, AGENT.replace("{dir}", &dir));
    let config = format!(
        "[agents.fake]\nbase = 'codex'\ncommand = ['{}']\ngate = ['{{prompt}}', '{{schema}}']\n\
         gate_resume = ['resume', '{{session}}', '{{prompt}}', '{{schema}}']\n\
         [agents.reviewer]\ncommand = ['{}']\nreview = ['{{prompt}}', '{{schema}}']\n\
         [gate]\nagent = '{gate}'\nescalate = ['a push to a tag']\n\
         [permissions]\npush = 'gate'\n",
        script.display(),
        script.display(),
    );
    let fixture = Fixture::new(root, &config).await;
    fixture.run_gate();
    fixture
}

impl Fixture {
    /// Have the gate return `verdict` from now on.
    fn verdict(&self, verdict: &str, reason: &str, rule: &str) {
        let output = serde_json::json!({"verdict": verdict, "reason": reason, "rule": rule});
        std::fs::write(self.root.path().join("verdict.json"), output.to_string()).unwrap();
    }

    /// The gate's runs so far, as the agent recorded them.
    fn runs(&self) -> Vec<String> {
        let runs = std::fs::read_to_string(self.root.path().join("runs")).unwrap_or_default();
        runs.lines().map(ToOwned::to_owned).collect()
    }

    /// Create or remove the file `name` the agent reads.
    fn mark(&self, name: &str, present: bool) {
        let path = self.root.path().join(name);
        match present {
            true => std::fs::write(path, "").unwrap(),
            false => std::fs::remove_file(path).unwrap(),
        }
    }

    /// Commit a change to the workspace's branch.
    fn change(&self, text: &str) {
        std::fs::write(self.workspace.path.join("tracked"), text).unwrap();
        git(&self.workspace.path, &["commit", "-qam", "tracked"]);
    }

    /// The agent's push of HEAD to `destination`, handed to the gate as
    /// approval routing does, unless the gate stopped hearing it.
    async fn push(&self, destination: &str) -> Result<ApprovalRequest> {
        let head = git(&self.workspace.path, &["rev-parse", "HEAD"]);
        let push = PushSpecification {
            remote: "origin".into(),
            url: None,
            destination: destination.into(),
            commit: Some(head.trim().into()),
            replaces: None,
            agent: None,
        };
        let request = ApprovalRequest::new(
            &self.workspace.id,
            Target::Push,
            &push.request_name(),
            Specification::Push(push),
            Lifetime::Lease,
            None,
        );
        let daemon: &dyn GateDaemon = &*self.daemon;
        self.reviews.ensure_gate_hears(daemon, &request).await?;
        let request = self.requests.add(request);
        assert!(self.reviews.give_gate(daemon, &request).await?);
        Ok(request)
    }

    /// The push of the workspace's own branch.
    async fn push_branch(&self) -> ApprovalRequest {
        self.push("refs/heads/fix-login").await.unwrap()
    }

    /// The request stored under `key` once `done` holds of it.
    async fn request_when(
        &self,
        key: &str,
        done: impl Fn(&ApprovalRequest) -> bool,
    ) -> ApprovalRequest {
        let mut changed = self.requests.changes();
        loop {
            if let Some(stored) = self.requests.get(key).filter(|stored| done(stored)) {
                return stored;
            }
            changed.changed().await.unwrap();
        }
    }

    /// The next message the workspace's agents receive.
    async fn message(&self) -> String {
        let mut changed = self.requests.changes();
        loop {
            let next = {
                let mut messages = self.daemon.messages.lock().unwrap();
                (!messages.is_empty()).then(|| messages.remove(0))
            };
            if let Some(next) = next {
                return next;
            }
            changed.changed().await.unwrap();
        }
    }

    /// The keys of the requests announced to whoever decides them, once
    /// there is one.
    async fn announced(&self) -> Vec<String> {
        let mut changed = self.requests.changes();
        loop {
            let announced = self.daemon.announced.lock().unwrap().clone();
            if !announced.is_empty() {
                return announced;
            }
            changed.changed().await.unwrap();
        }
    }

    /// What the user was told that the gate could not be set up.
    fn unavailable(&self) -> usize {
        let notified = self.daemon.notified.lock().unwrap();
        (notified.iter())
            .filter(|(kind, _)| *kind == NotificationKind::GateUnavailable)
            .count()
    }

    /// The verdicts the workspace's log holds.
    async fn logged_verdicts(&self) -> Vec<String> {
        let id = self.workspace.id.clone();
        self.reviews
            .store
            .read(move |tx| {
                let mut statement = tx.prepare(
                    "SELECT json_extract(record,'$.gate.verdict') FROM space_events
                     WHERE space_id=?1 AND json_extract(record,'$.kind')='gate_decided'
                     ORDER BY id",
                )?;
                let verdicts = statement.query_map([&id], |row| row.get(0))?;
                Ok(verdicts.collect::<rusqlite::Result<Vec<String>>>()?)
            })
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn an_allowed_request_is_approved_without_the_user() {
    let fixture = fixture().await;
    fixture.verdict("allow", "", "");
    let request = fixture.push_branch().await;
    let approved = fixture
        .request_when(&request.key, |stored| {
            stored.status == DecisionStatus::Approved
        })
        .await;
    assert_eq!(approved.gate, Some(GateStep::Allowed));
    assert_eq!(fixture.logged_verdicts().await, ["allow"]);
    assert!(fixture.daemon.announced.lock().unwrap().is_empty());
}

/// A run's `shoal` commands reach the daemon with a read-only scope of the
/// requesting space, which ends with the run.
#[tokio::test]
async fn a_run_holds_a_read_only_scope_of_the_space_until_it_ends() {
    let fixture = fixture().await;
    fixture.verdict("allow", "", "");
    fixture.mark("hold", true);
    let request = fixture.push_branch().await;
    let path = fixture.root.path().join("scope");
    while !path.exists() {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let scope = std::fs::read_to_string(&path).unwrap();
    let (token, state) = scope.split_once('\n').unwrap();
    assert_eq!(std::path::Path::new(state), fixture.reviews.paths.state);
    let issued = |live| {
        let scopes = fixture.daemon.scopes.lock().unwrap();
        (scopes.iter())
            .filter(|(issued, caller, is_live)| {
                issued.expose() == token && caller.read_only && *is_live == live
            })
            .count()
    };
    assert_eq!(issued(true), 1);
    fixture.mark("hold", false);
    fixture
        .request_when(&request.key, |stored| {
            stored.status == DecisionStatus::Approved
        })
        .await;
    assert_eq!(issued(false), 1);
}

#[tokio::test]
async fn an_allow_that_cannot_be_carried_out_takes_the_normal_route() {
    let fixture = fixture().await;
    fixture.verdict("allow", "", "");
    fixture.daemon.approve_fails.store(true, Ordering::SeqCst);
    let request = fixture.push_branch().await;
    let message = fixture.message().await;
    assert!(message.contains("carrying it out failed"), "{message}");
    let stored = fixture.requests.get(&request.key).unwrap();
    assert!(matches!(stored.gate, Some(GateStep::Unapplied { .. })));
    assert_eq!(fixture.announced().await, [request.key]);
}

#[tokio::test]
async fn an_allow_a_restart_cut_short_is_carried_out() {
    let fixture = fixture().await;
    fixture.verdict("deny", "Too late.", "");
    fixture.mark("hold", true);
    let request = fixture.push_branch().await;
    // The gate allowed it, and the daemon stopped before its approval.
    let (key, requests) = (request.key.clone(), fixture.requests.clone());
    let allowed = fixture
        .reviews
        .store
        .write(move |tx| requests.allow(tx, &key));
    allowed.await.unwrap();
    let daemon: &dyn GateDaemon = &*fixture.daemon;
    fixture.reviews.carry_out_stranded_allows(daemon).await;
    let stored = fixture.requests.get(&request.key).unwrap();
    assert_eq!(stored.status, DecisionStatus::Approved);
    fixture.mark("hold", false);
}

#[tokio::test]
async fn a_gate_that_cannot_be_set_up_denies_and_tells_the_user() {
    let fixture = fixture_deciding_by("absent").await;
    fixture.push_branch().await;
    let denied = fixture.message().await;
    assert!(denied.contains("could not decide"), "{denied}");
    assert_eq!(fixture.unavailable(), 1);
    assert!(fixture.runs().is_empty());
}

/// A tool that only reviews has no template to run the gate with, which
/// only the config fixes.
#[tokio::test]
async fn a_tool_without_a_gate_template_cannot_be_the_gate() {
    let fixture = fixture_deciding_by("reviewer").await;
    fixture.push_branch().await;
    fixture.message().await;
    assert_eq!(fixture.unavailable(), 1);
}

#[tokio::test]
async fn a_run_that_panics_denies_its_request() {
    let fixture = fixture().await;
    fixture.mark("hold", true);
    let request = fixture.push_branch().await;
    let mut runs = Runs::default();
    runs.spawn(request, async { panic!("the gate's run broke") });
    let ended = runs.running.join_next_with_id().await.unwrap();
    runs.end(&fixture.reviews, &*fixture.daemon, ended).await;
    let denied = fixture.message().await;
    assert!(denied.contains("could not decide"), "{denied}");
    assert_eq!(fixture.logged_verdicts().await, ["failed"]);
    fixture.mark("hold", false);
}

#[tokio::test]
async fn a_request_whose_space_is_gone_takes_the_normal_route() {
    let fixture = fixture().await;
    fixture.mark("hold", true);
    let request = fixture.push_branch().await;
    fixture.daemon.space_gone.store(true, Ordering::SeqCst);
    fixture.mark("hold", false);
    let passed = fixture
        .request_when(&request.key, |stored| {
            matches!(stored.gate, Some(GateStep::Unjudged { .. }))
        })
        .await;
    assert_eq!(passed.status, DecisionStatus::Pending);
    assert_eq!(fixture.announced().await, [request.key]);
}

#[tokio::test]
async fn a_session_that_cannot_be_resumed_gives_way_to_a_new_one() {
    let fixture = fixture().await;
    fixture.verdict("deny", "Add a test.", "");
    fixture.push_branch().await;
    fixture.message().await;
    fixture.mark("stale", true);
    fixture.change("tested\n");
    fixture.push_branch().await;
    let denied = fixture.message().await;
    assert!(denied.contains("Add a test."), "{denied}");
    fixture.mark("stale", false);
    fixture.change("tested again\n");
    fixture.push_branch().await;
    fixture.message().await;
    // The new session replaced the one that could not be resumed.
    assert_eq!(fixture.runs(), ["new", "resume s1", "new", "resume s3"]);
}

#[tokio::test]
async fn a_denial_reaches_the_agent_and_the_third_of_one_commit_stops_asking() {
    let fixture = fixture().await;
    // A failure denies without counting toward the stop.
    fixture.mark("fail", true);
    fixture.push_branch().await;
    let failed = fixture.message().await;
    assert!(failed.contains("could not decide"), "{failed}");
    fixture.mark("fail", false);
    fixture.verdict("deny", "Squash the fixup commits.", "");
    for _ in 0..DENIALS {
        fixture.push_branch().await;
        let denied = fixture.message().await;
        assert!(denied.contains("Squash the fixup commits."), "{denied}");
    }
    let refused = fixture.push("refs/heads/fix-login").await.unwrap_err();
    assert!(format!("{refused:#}").contains("Squash the fixup commits."));
    assert_eq!(fixture.runs().len(), 1 + DENIALS as usize);
    // A new commit is a new action, which the gate hears.
    fixture.change("squashed\n");
    fixture.push_branch().await;
    fixture.message().await;
    assert_eq!(fixture.runs().len(), 2 + DENIALS as usize);
    assert!(fixture.daemon.announced.lock().unwrap().is_empty());
    let logged = fixture.logged_verdicts().await;
    assert_eq!(logged.first().map(String::as_str), Some("failed"));
    assert_eq!(logged.len(), 2 + DENIALS as usize);
}

#[tokio::test]
async fn an_escalation_quoting_a_case_takes_the_normal_route() {
    let fixture = fixture().await;
    fixture.verdict("escalate", "It moves v1.", "a push to a tag");
    let request = fixture.push("refs/tags/v1").await.unwrap();
    let escalated = fixture
        .request_when(&request.key, |stored| {
            matches!(stored.gate, Some(GateStep::Escalated { .. }))
        })
        .await;
    assert_eq!(escalated.status, DecisionStatus::Pending);
    let Some(GateStep::Escalated { reason, rule }) = escalated.gate else {
        unreachable!()
    };
    assert_eq!(
        (reason.as_str(), rule.as_str()),
        ("It moves v1.", "a push to a tag")
    );
    let message = fixture.message().await;
    assert!(message.contains("It moves v1."), "{message}");
    assert_eq!(fixture.announced().await, [request.key]);
}

#[tokio::test]
async fn an_escalation_quoting_no_case_is_asked_again_then_denied() {
    let fixture = fixture().await;
    fixture.verdict("escalate", "Not sure.", "anything unclear");
    fixture.push_branch().await;
    let denied = fixture.message().await;
    assert!(denied.contains("could not decide"), "{denied}");
    // The second run continued the first's session, told why.
    assert_eq!(fixture.runs(), ["new", "resume s1"]);
    assert!(fixture.daemon.announced.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_subject_resumes_its_session_and_another_subject_starts_one() {
    let fixture = fixture().await;
    fixture.verdict("deny", "Add a test.", "");
    fixture.push_branch().await;
    fixture.message().await;
    fixture.change("tested\n");
    fixture.push_branch().await;
    fixture.message().await;
    fixture.push("refs/heads/other").await.unwrap();
    fixture.message().await;
    assert_eq!(fixture.runs(), ["new", "resume s1", "new"]);
    // A resumed session is given the space's issues and PRs as they are now.
    let resumed = std::fs::read_to_string(fixture.root.path().join("resumed")).unwrap();
    assert!(resumed.contains("issues and PRs now"), "{resumed}");
}

#[tokio::test]
async fn a_gate_run_in_the_repository_directory_holds_back_its_repository() {
    let fixture = fixture().await;
    let (reviews, daemon) = (&fixture.reviews, &*fixture.daemon);
    let repository = &fixture.repository.id;
    reviews
        .ensure_no_gate_runs_in(daemon, repository)
        .await
        .unwrap();
    let runs = ShoalDir::of(&fixture.repository, Contents::GateRuns);
    runs.create().unwrap();
    let run = runs.root().join("request");
    std::fs::create_dir_all(&run).unwrap();
    assert!(
        reviews
            .ensure_no_gate_runs_in(daemon, repository)
            .await
            .is_err()
    );
    std::fs::remove_dir_all(&run).unwrap();
    reviews
        .ensure_no_gate_runs_in(daemon, repository)
        .await
        .unwrap();
}
