use super::*;
use crate::daemon::{
    test_support::{manager, repository},
    workspace_files::Attacher,
};
use shoal_protocol::space_names::SpaceSelector;

#[tokio::test]
async fn long_file_listings_go_in_pages() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
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
                    "INSERT INTO space_files
                        (space_id,space,repository_id,source,name,path,bytes,created_at,
                            attached_by)
                        VALUES (?1,'receiver',?2,'added','notes.md',?3,1,0,'{\"kind\":\"user\"}')",
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
            space: SpaceSelector::id(&workspace.id),
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
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "receiver".into(), None, None, None)
        .await
        .unwrap();
    let file = temp.path().join("notes.md");
    std::fs::write(&file, "plan").unwrap();
    manager
        .add_attached_files(
            &SpaceSelector::id(&workspace.id),
            vec![file.into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    let refused = handle(
        &manager,
        FileMethod::RemoveFiles {
            space: None,
            names: vec!["notes.md".into()],
            ids: vec![],
        },
        None,
    )
    .await;
    assert!(
        refused.is_err(),
        "deleting by name without a workspace was accepted"
    );
    assert_eq!(
        manager
            .attached_files(&SpaceSelector::id(&workspace.id))
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn a_chats_agent_lists_and_deletes_its_own_chats_files() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = crate::daemon::test_support::chat(&manager, "talk", "chat", Some(&repo), None).await;
    let token = manager.chat_scope(&chat.id).await.unwrap();
    for method in [
        FileMethod::ListFiles {
            space: SpaceSelector::id(&chat.id),
        },
        FileMethod::RemoveFiles {
            space: Some(SpaceSelector::id(&chat.id)),
            names: Vec::new(),
            ids: Vec::new(),
        },
    ] {
        let mut method = crate::protocol::Operation::File(method);
        crate::daemon::scope::authorize(&manager, Some(&token), &mut method)
            .await
            .unwrap();
    }
}
