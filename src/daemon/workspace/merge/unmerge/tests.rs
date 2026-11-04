use std::{fs, path::PathBuf, sync::Arc};

use crate::{
    daemon::{
        approvals::{DecisionStatus, Specification, Undecided},
        scope,
        workspace::{ExecutionKind, ExecutionRequest, Manager, RecordChange},
    },
    model::{MergeOptions, Workspace},
    test_support::{commit, git, manager, repository},
};

struct Fixture {
    _root: tempfile::TempDir,
    repo: PathBuf,
    manager: Arc<Manager>,
    repo_id: String,
}

impl Fixture {
    async fn new() -> Self {
        let (root, manager) = manager().await;
        let repo = repository(root.path(), "repo");
        let repo_id = manager
            .register_repository(repo.to_str().unwrap().into(), None, None)
            .await
            .unwrap()
            .id;
        Self {
            _root: root,
            repo,
            manager,
            repo_id,
        }
    }

    /// A workspace whose branch has one commit writing `file`.
    async fn add(&self, name: &str, file: &str) -> Workspace {
        let workspace = self
            .manager
            .create_workspace(&self.repo_id, name.into(), None, None, None)
            .await
            .unwrap();
        write_commit(&workspace.path, file, name);
        workspace
    }

    /// Merge the workspace `name` and record it, as a merge execution does.
    async fn merge(&self, name: &str) {
        let _guard = self.manager.gates.lock_git(&self.repo_id).await;
        let plan = self
            .manager
            .prepare_merge(name, &MergeOptions::default())
            .await
            .unwrap();
        crate::git::repo::finish_merge(plan.clone()).await.unwrap();
        let change = RecordChange::Merge(Box::new(plan));
        self.manager.update_merge_records(&change).await;
    }

    fn main(&self) -> String {
        rev(&self.repo, "main")
    }
}

fn rev(repo: &std::path::Path, revision: &str) -> String {
    git(repo, &["rev-parse", revision]).trim().to_owned()
}

fn write_commit(path: &std::path::Path, file: &str, text: &str) {
    fs::write(path.join(file), format!("{text}\n")).unwrap();
    commit(path, file);
}

#[tokio::test]
async fn an_agents_unmerge_waits_for_approval_of_the_merge_it_undoes() {
    let f = Fixture::new().await;
    let worker = f.add("worker", "feature").await;
    let before = f.main();
    f.merge("worker").await;
    let result = f.main();
    let caller = scope::Caller {
        execution: None,
        space_id: worker.id.clone(),
    };
    let begin = async || {
        f.manager
            .begin_execution_placed(
                &worker.id,
                None,
                ExecutionKind::Unmerge,
                Some(&caller),
                ExecutionRequest::default(),
            )
            .await
    };
    let error = begin().await.expect_err("unmerge needs a decision");
    let pending = error
        .downcast::<Undecided>()
        .expect("an approval request")
        .0;
    assert_eq!(pending.status, DecisionStatus::Pending);
    assert!(
        matches!(&pending.specification, Specification::Unmerge(unmerge) if unmerge.result == result)
    );
    assert_eq!(
        pending.subject(),
        format!("undo the merge of worker at {result} ({result})")
    );
    assert_eq!(f.main(), result);
    f.manager
        .decide_approval(pending.number, true, None, Default::default(), None)
        .await
        .unwrap();
    let started = begin().await.unwrap();
    let plan = started.plan.unmerge.as_ref().unwrap();
    assert_eq!(
        (plan.previous.as_str(), plan.tip.as_str()),
        (before.as_str(), result.as_str())
    );
    f.manager
        .finish_execution(started.plan.id.clone(), ExecutionKind::Unmerge, Some(1))
        .await
        .unwrap();
}
