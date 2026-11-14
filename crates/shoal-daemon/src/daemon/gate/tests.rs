use std::sync::Arc;

use super::*;
use crate::client::space_names::SpaceSelector;
use crate::client::{links::PushOutcome, notifications::NotificationKind};
use crate::daemon::{
    approvals::PushSpecification,
    scope::Caller,
    test_support::{commit, configure, install_test_script, manager, repository},
};
use crate::model::Workspace;

/// A fake gate agent: it records each run as `new` or `resume <session>`,
/// the latest resumed run's prompt in `resumed`,
/// reports session `s<run>` or the one it resumes, fails while `fail`
/// exists, cannot resume while `stale` exists, waits while `hold` exists, and returns `verdict.json`.
const AGENT: &str = r#"#!/bin/sh
dir='{dir}'
if [ "$1" = resume ]; then echo "resume $2" >> "$dir/runs"; session=$2; printf '%s' "$3" > "$dir/resumed"
else echo new >> "$dir/runs"; session="s$(wc -l < "$dir/runs" | tr -d ' ')"; fi
echo "session id: $session" >&2
[ -e "$dir/fail" ] && exit 1
[ "$1" = resume ] && [ -e "$dir/stale" ] && exit 1
while [ -e "$dir/hold" ]; do sleep 0.1; done
cat "$dir/verdict.json"
"#;

struct Fixture {
    root: tempfile::TempDir,
    manager: Arc<Manager>,
    workspace: Workspace,
    caller: Caller,
}

/// A workspace whose pushes the gate decides, by the fake [`AGENT`], with
/// `a push to a tag` as the one case it passes on.
async fn fixture() -> Fixture {
    fixture_deciding_by("fake").await
}

/// [`fixture`] with `gate` as `[gate] agent`.
async fn fixture_deciding_by(gate: &str) -> Fixture {
    let (root, manager) = manager().await;
    let script = root.path().join("gate.sh");
    let dir = root.path().to_string_lossy();
    install_test_script(&script, &AGENT.replace("{dir}", &dir));
    let config = format!(
        "[agents.fake]\nbase = 'codex'\ncommand = ['{}']\nreview = ['{{prompt}}', '{{schema}}']\n\
         review_resume = ['resume', '{{session}}', '{{prompt}}', '{{schema}}']\n\
         [gate]\nagent = '{gate}'\nescalate = ['a push to a tag']\n\
         [permissions]\npush = 'gate'\n\
         [machine]\nmax_agents = 4\n[machine.overload.disk]\nenabled = false\n",
        script.display(),
    );
    configure(&manager, &config).await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    tokio::spawn(run(manager.clone()));
    tokio::spawn(crate::daemon::queued_starts::run(manager.clone()));
    let caller = Caller {
        space_id: workspace.id.clone(),
        execution: None,
        granted: None,
    };
    Fixture {
        root,
        manager,
        workspace,
        caller,
    }
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

    /// Commit a change to the workspace's branch.
    fn change(&self, text: &str) {
        std::fs::write(self.workspace.path.join("tracked"), text).unwrap();
        commit(&self.workspace.path, "tracked");
    }

    /// The agent's push of HEAD to `destination`, as a request.
    async fn push(&self, destination: &str) -> anyhow::Result<ApprovalRequest> {
        let head = crate::git::run(&self.workspace.path, &["rev-parse", "HEAD"]).await?;
        let push = PushSpecification {
            remote: "origin".into(),
            url: None,
            destination: destination.into(),
            commit: Some(head.trim().into()),
            replaces: None,
            agent: None,
        };
        let selector = SpaceSelector::id(&self.workspace.id);
        match (self
            .manager
            .request_push(&selector, push, Some(&self.caller)))
        .await?
        {
            PushOutcome::Proposed(proposed) => Ok(*proposed.proposed),
            PushOutcome::Direct => panic!("expected the push to wait for the gate"),
        }
    }

    /// The push of the workspace's own branch.
    async fn push_branch(&self) -> ApprovalRequest {
        self.push("refs/heads/worker").await.unwrap()
    }

    /// The next message the workspace's agent receives, which it takes as
    /// its hooks would, so the next one is not collapsed into it.
    async fn message(&self) -> String {
        let selector = SpaceSelector::id(&self.workspace.id);
        let mut changed = self.manager.store.watch_changes();
        loop {
            let received = self.manager.agent_messages(&selector).await.unwrap();
            if let Some(next) = received.into_iter().next() {
                (self
                    .manager
                    .mark_agent_messages_delivered(&selector, vec![next.id]))
                .await
                .unwrap();
                return next.message;
            }
            changed.changed().await.unwrap();
        }
    }

    /// The request stored under `key` once `done` holds of it.
    async fn request_when(
        &self,
        key: &str,
        done: impl Fn(&ApprovalRequest) -> bool,
    ) -> ApprovalRequest {
        let mut changed = self.manager.store.watch_changes();
        loop {
            let stored = self.manager.approval_by_key(key).await.unwrap();
            if let Some(stored) = stored.filter(|stored| done(stored)) {
                return stored;
            }
            changed.changed().await.unwrap();
        }
    }

    /// The user's approval notifications.
    async fn asked_user(&self) -> Vec<String> {
        let notifications = self.manager.notifications(false, 100).await.unwrap();
        (notifications.into_iter())
            .filter(|notification| notification.kind == NotificationKind::ApprovalRequested)
            .map(|notification| notification.message)
            .collect()
    }

    /// The verdicts the workspace's log holds.
    async fn logged_verdicts(&self) -> Vec<String> {
        let id = self.workspace.id.clone();
        self.manager
            .store
            .read(move |tx| {
                let mut statement = tx.prepare(
                    "SELECT json_extract(record,'$.gate.verdict') FROM workspace_events
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
async fn an_allowed_push_runs_as_an_approved_one_without_the_user() {
    let fixture = fixture().await;
    fixture.verdict("allow", "", "");
    let request = fixture.push_branch().await;
    let allowed = fixture
        .request_when(&request.key, |stored| stored.operation.is_some())
        .await;
    assert_eq!(allowed.gate, Some(GateStep::Allowed));
    assert_eq!(fixture.logged_verdicts().await, ["allow"]);
}

#[tokio::test]
async fn an_allowed_request_that_cannot_be_carried_out_goes_to_the_user() {
    let fixture = fixture().await;
    fixture.verdict("allow", "", "");
    std::fs::write(fixture.root.path().join("hold"), "").unwrap();
    let request = fixture.push_branch().await;
    // The workspace's worktree goes while the gate decides, so the push
    // its approval starts cannot run.
    let moved = fixture.workspace.path.with_extension("moved");
    std::fs::rename(&fixture.workspace.path, &moved).unwrap();
    std::fs::remove_file(fixture.root.path().join("hold")).unwrap();
    let mut changed = fixture.manager.store.watch_changes();
    let asked = loop {
        let asked = fixture.asked_user().await;
        if !asked.is_empty() {
            break asked;
        }
        changed.changed().await.unwrap();
    };
    assert!(asked[0].contains(&request.number.to_string()), "{asked:?}");
    let message = fixture.message().await;
    assert!(message.contains("carrying it out failed"), "{message}");
    // A restart leaves it to the decider it went to.
    fixture.manager.carry_out_stranded_allows().await;
    let stored = fixture.manager.approval_by_key(&request.key).await.unwrap();
    let stored = stored.unwrap();
    assert!(matches!(stored.gate, Some(GateStep::Unapplied { .. })));
    assert_eq!(fixture.asked_user().await.len(), 1);
}

#[tokio::test]
async fn an_allow_a_restart_cut_short_is_carried_out() {
    let fixture = fixture().await;
    fixture.verdict("deny", "Too late.", "");
    std::fs::write(fixture.root.path().join("hold"), "").unwrap();
    let request = fixture.push_branch().await;
    let key = request.key.clone();
    // The gate allowed it, and the daemon stopped before its approval.
    let allowed = fixture
        .manager
        .store
        .write(move |tx| crate::daemon::approvals::gated::allow(tx, &key))
        .await
        .unwrap();
    fixture.manager.carry_out_stranded_allows().await;
    let started = fixture
        .request_when(&allowed.key, |stored| stored.operation.is_some())
        .await;
    assert_eq!(started.gate, Some(GateStep::Allowed));
    std::fs::remove_file(fixture.root.path().join("hold")).unwrap();
}

#[tokio::test]
async fn a_gate_that_cannot_be_set_up_denies_and_tells_the_user() {
    let fixture = fixture_deciding_by("absent").await;
    fixture.push_branch().await;
    let denied = fixture.message().await;
    assert!(denied.contains("could not decide"), "{denied}");
    let notifications = fixture.manager.notifications(false, 100).await.unwrap();
    let unavailable = (notifications.iter())
        .filter(|notification| notification.kind == NotificationKind::GateUnavailable)
        .count();
    assert_eq!(unavailable, 1);
    assert!(fixture.runs().is_empty());
}

#[tokio::test]
async fn an_issue_proposal_whose_space_is_gone_goes_to_the_user() {
    let fixture = fixture().await;
    let space = fixture.workspace.clone().into();
    let request =
        crate::daemon::links::proposals::tests::store_issue_proposal(&fixture.manager, &space)
            .await;
    let manager = &fixture.manager;
    manager
        .remove_workspace(
            &SpaceSelector::id(&fixture.workspace.id),
            crate::removal::UnsavedWork::default(),
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    // The proposal outlives its space, and reaches the gate after it went.
    let key = request.key.clone();
    let given = manager
        .store
        .write(move |tx| crate::daemon::approvals::gated::give_gate(tx, &key));
    given.await.unwrap();
    manager.gate.queued.notify_one();
    let passed = fixture
        .request_when(&request.key, |stored| {
            matches!(stored.gate, Some(GateStep::Unjudged { .. }))
        })
        .await;
    assert_eq!(passed.status, DecisionStatus::Pending);
    assert!(fixture.runs().is_empty());
}

#[tokio::test]
async fn a_session_that_cannot_be_resumed_gives_way_to_a_new_one() {
    let fixture = fixture().await;
    fixture.verdict("deny", "Add a test.", "");
    fixture.push_branch().await;
    fixture.message().await;
    std::fs::write(fixture.root.path().join("stale"), "").unwrap();
    fixture.change("tested\n");
    fixture.push_branch().await;
    let denied = fixture.message().await;
    assert!(denied.contains("Add a test."), "{denied}");
    std::fs::remove_file(fixture.root.path().join("stale")).unwrap();
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
    std::fs::write(fixture.root.path().join("fail"), "").unwrap();
    fixture.push_branch().await;
    let failed = fixture.message().await;
    assert!(failed.contains("could not decide"), "{failed}");
    std::fs::remove_file(fixture.root.path().join("fail")).unwrap();
    fixture.verdict("deny", "Squash the fixup commits.", "");
    for _ in 0..DENIALS {
        fixture.push_branch().await;
        let denied = fixture.message().await;
        assert!(denied.contains("Squash the fixup commits."), "{denied}");
    }
    let refused = fixture.push("refs/heads/worker").await.unwrap_err();
    assert!(format!("{refused:#}").contains("Squash the fixup commits."));
    assert_eq!(fixture.runs().len(), 1 + DENIALS as usize);
    // A new commit is a new action, which the gate hears.
    fixture.change("squashed\n");
    fixture.push_branch().await;
    fixture.message().await;
    assert_eq!(fixture.runs().len(), 2 + DENIALS as usize);
    assert!(fixture.asked_user().await.is_empty());
    let logged = fixture.logged_verdicts().await;
    assert_eq!(logged.first().map(String::as_str), Some("failed"));
    assert_eq!(logged.len(), 2 + DENIALS as usize);
}

#[tokio::test]
async fn an_escalation_quoting_a_case_reaches_the_user_with_the_gates_reason() {
    let fixture = fixture().await;
    fixture.verdict("escalate", "It moves v1.", "a push to a tag");
    let request = fixture.push("refs/tags/v1").await.unwrap();
    let escalated = fixture
        .request_when(&request.key, |stored| {
            matches!(stored.gate, Some(GateStep::Escalated { .. }))
        })
        .await;
    assert_eq!(escalated.status, DecisionStatus::Pending);
    let mut changed = fixture.manager.store.watch_changes();
    let asked = loop {
        let asked = fixture.asked_user().await;
        if !asked.is_empty() {
            break asked;
        }
        changed.changed().await.unwrap();
    };
    assert!(asked[0].contains("It moves v1."), "{asked:?}");
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
    assert!(fixture.asked_user().await.is_empty());
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
async fn a_gate_run_in_the_repository_directory_holds_back_its_move_and_removal() {
    let fixture = fixture().await;
    let manager = &fixture.manager;
    let repository = fixture.workspace.repository_id.clone();
    manager.ensure_no_gate_runs_in(&repository).await.unwrap();
    let registered = manager.repository(&repository).await.unwrap();
    let runs = crate::shoal_dirs::ShoalDir::of(&registered, crate::shoal_dirs::Contents::GateRuns)
        .unwrap();
    runs.create().unwrap();
    let run = runs.root().join("request");
    std::fs::create_dir_all(&run).unwrap();
    assert!(manager.ensure_no_gate_runs_in(&repository).await.is_err());
    assert!(manager.remove_repository(&repository).await.is_err());
    assert!(run.exists());
    std::fs::remove_dir_all(&run).unwrap();
    manager.ensure_no_gate_runs_in(&repository).await.unwrap();
}
