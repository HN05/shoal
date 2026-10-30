use super::*;
use crate::{
    daemon::workspace_files::Attacher,
    test_support::{manager, repository},
};

#[tokio::test]
async fn long_file_listings_go_in_pages() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "receiver".into(), None, None, None)
        .await
        .unwrap();
    // More records than one frame holds.
    let (id, repository_id) = (workspace.id.clone(), repo.id.clone());
    manager
        .store
        .run(move |db| {
            for n in 0..1000 {
                db.execute(
                    "INSERT INTO workspace_files
                        (owner_id,owner,repository_id,source,name,path,bytes,created_at)
                        VALUES (?1,'receiver',?2,'added','notes.md',?3,1,0)",
                    rusqlite::params![id, repository_id, format!("/attachments/{n:0>64}/notes.md")],
                )?;
            }
            Ok(())
        })
        .await
        .unwrap();
    let answer = handle(
        &manager,
        FileMethod::ListFiles {
            workspace: workspace.name.clone(),
        },
        None,
    )
    .await
    .unwrap();
    let Answer::Pages(Ok(pages)) = answer else {
        panic!("expected pages");
    };
    assert!(pages.len() > 1, "{} pages", pages.len());
    let listed: usize = pages
        .into_iter()
        .map(|page| {
            Vec::<crate::daemon::workspace_files::AttachedFile>::try_from(page)
                .unwrap()
                .len()
        })
        .sum();
    assert_eq!(listed, 1000);
}

#[tokio::test]
async fn deleting_by_name_needs_a_workspace() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "receiver".into(), None, None, None)
        .await
        .unwrap();
    let file = temp.path().join("notes.md");
    std::fs::write(&file, "plan").unwrap();
    manager
        .add_attached_files(&workspace.name, vec![file.into()], &Attacher::User)
        .await
        .unwrap();
    let refused = handle(
        &manager,
        FileMethod::RemoveFiles {
            workspace: None,
            names: vec!["notes.md".into()],
            ids: vec![],
        },
        None,
    )
    .await;
    let Err(error) = refused else {
        panic!("deleting by name without a workspace was accepted");
    };
    assert!(
        error.to_string().contains("name the workspace"),
        "{error:#}"
    );
    assert_eq!(
        manager.attached_files(&workspace.name).await.unwrap().len(),
        1
    );
}
