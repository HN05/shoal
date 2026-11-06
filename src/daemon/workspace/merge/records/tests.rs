use std::fs;

use crate::{
    daemon::test_support::{commit, git, manager, repository},
    model::MergeOptions,
};

/// The recorded merges of the repository, oldest first, as
/// `(branch, source, previous, result)`.
fn recorded(db: &rusqlite::Connection) -> anyhow::Result<Vec<[String; 4]>> {
    let mut statement = db.prepare(
        "SELECT branch,source_commit,previous_commit,result_commit FROM merges ORDER BY id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok([row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?])
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

#[tokio::test]
async fn a_merge_is_recorded_from_what_the_default_branch_holds() {
    let (root, manager) = manager().await;
    let repo = repository(root.path(), "repo");
    let repo_id = manager
        .register_repository(repo.to_str().unwrap().into(), None, None)
        .await
        .unwrap()
        .id;
    let workspace = manager
        .create_workspace(&repo_id, "worker".into(), None, None, None)
        .await
        .unwrap();
    let merge = async || {
        let _guard = manager.gates.lock_git(&repo_id).await;
        let plan = manager
            .prepare_merge("worker", &MergeOptions::default())
            .await
            .unwrap();
        crate::git::repo::finish_merge(plan.clone()).await.unwrap();
        manager.record_merge(&plan).await.unwrap();
    };
    // A branch the default branch already contains merges nothing.
    merge().await;
    assert!(
        manager
            .store
            .run(|db| recorded(db))
            .await
            .unwrap()
            .is_empty()
    );

    let main = git(&repo, &["rev-parse", "main"]).trim().to_owned();
    fs::write(workspace.path.join("feature"), "work\n").unwrap();
    commit(&workspace.path, "feature");
    let head = git(&workspace.path, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    merge().await;
    let fast_forward = [
        "worker".to_owned(),
        head.clone(),
        main.clone(),
        head.clone(),
    ];
    assert_eq!(
        manager.store.run(|db| recorded(db)).await.unwrap(),
        std::slice::from_ref(&fast_forward)
    );

    fs::write(repo.join("tracked"), "main side\n").unwrap();
    commit(&repo, "tracked");
    let main = git(&repo, &["rev-parse", "main"]).trim().to_owned();
    fs::write(workspace.path.join("feature"), "more\n").unwrap();
    commit(&workspace.path, "feature");
    let head = git(&workspace.path, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    merge().await;
    let result = git(&repo, &["rev-parse", "main"]).trim().to_owned();
    assert_eq!(git(&repo, &["rev-parse", "main^2"]).trim(), head);
    let merge_commit = ["worker".to_owned(), head, main, result.clone()];
    assert_eq!(
        manager.store.run(|db| recorded(db)).await.unwrap(),
        [fast_forward.clone(), merge_commit.clone()]
    );

    // A commit made on main before the record is not part of the merge.
    fs::write(workspace.path.join("feature"), "last\n").unwrap();
    commit(&workspace.path, "feature");
    let head = git(&workspace.path, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    let _guard = manager.gates.lock_git(&repo_id).await;
    let plan = manager
        .prepare_merge("worker", &MergeOptions::default())
        .await
        .unwrap();
    crate::git::repo::finish_merge(plan.clone()).await.unwrap();
    let merged = git(&repo, &["rev-parse", "main"]).trim().to_owned();
    fs::write(repo.join("later"), "after the merge\n").unwrap();
    commit(&repo, "later");
    manager.record_merge(&plan).await.unwrap();
    assert_eq!(
        manager.store.run(|db| recorded(db)).await.unwrap(),
        [
            fast_forward,
            merge_commit,
            ["worker".to_owned(), head, result, merged]
        ]
    );
}
