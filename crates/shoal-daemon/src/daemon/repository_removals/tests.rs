use std::fs;

use super::*;
use crate::daemon::test_support::{git, manager, repository};

#[tokio::test]
async fn a_preview_lists_each_space_with_the_work_its_removal_loses() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = (manager.repositories)
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let create = |name: &str| {
        let manager = manager.clone();
        let (id, name) = (repo.id.clone(), name.to_owned());
        async move {
            (manager.create_workspace(&id, name, None, None, None))
                .await
                .unwrap()
        }
    };
    let (worked, clean) = (create("worked").await, create("clean").await);
    for commit in ["one", "two"] {
        fs::write(worked.path.join(commit), commit).unwrap();
        git(&worked.path, &["add", commit]);
        git(&worked.path, &["commit", "-q", "-m", commit]);
    }
    fs::write(worked.path.join("notes.txt"), "draft").unwrap();
    let preview = manager
        .preview_repository_removal((&repo.id).into(), None, None)
        .await
        .unwrap();
    assert_eq!(preview.repository.id, repo.id);
    let work = |id: &shoal_ids::space_ids::SpaceId| {
        (preview.spaces.iter())
            .find(|space| &space.space.id == id)
            .and_then(|space| space.work)
            .unwrap()
    };
    let worked = work(&worked.id);
    assert!(worked.dirty);
    assert_eq!(worked.unpushed_commits, 2);
    assert_eq!(
        work(&clean.id),
        WorktreeWork {
            dirty: false,
            unpushed_commits: 0
        }
    );
}
