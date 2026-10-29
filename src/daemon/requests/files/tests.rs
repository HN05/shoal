use super::*;
use crate::test_support::{manager, repository};

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
                        (workspace_id,workspace,repository_id,source,name,path,bytes,created_at)
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
