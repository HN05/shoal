use crate::gates::GuardMode;
use crate::model::Completion;
use crate::model::HandedBack;
use crate::spaces::done::*;
use crate::stand_in::Fixture;
use shoal_core::test_support::{install_test_script, repository};
use shoal_protocol::assignments::Done;
use shoal_protocol::space_names::SpaceSelector;

use crate::model::Workspace;
use crate::workspace_files::Attacher;
use std::fs;

async fn fixture() -> (Fixture, Workspace) {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "finished".into(), None, None, None)
        .await
        .unwrap();
    (manager, workspace)
}

#[tokio::test]
async fn completion_without_a_hook_allows_concurrent_resource_hooks() {
    let (manager, workspace) = fixture().await;
    manager
        .write_local_config(
            workspace.repository(),
            Some("[cleanup.done]\nenabled = false\n".into()),
        )
        .await
        .unwrap();
    let resources = manager
        .gates
        .resource_guard(&workspace.id, GuardMode::Exclusive)
        .unwrap();
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    manager
        .write_local_config(
            workspace.repository(),
            Some("hooks.post_done = '/usr/bin/true'\n[cleanup.done]\nenabled = false\n".into()),
        )
        .await
        .unwrap();
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done {
                cleanup: true,
                ..Done::default()
            },
            &Attacher::User,
        )
        .await
        .unwrap_err();
    assert!(
        !manager
            .spaces
            .completion(&workspace.id)
            .await
            .unwrap()
            .unwrap()
            .cleanup
    );
    drop(resources);
    assert!(
        manager
            .spaces
            .mark_space_done(
                &*manager,
                &SpaceSelector::id(&workspace.id),
                Done {
                    cleanup: true,
                    ..Done::default()
                },
                &Attacher::User
            )
            .await
            .unwrap()
            .cleanup
    );
}

#[tokio::test]
async fn repeating_a_completion_reruns_a_configured_hook() {
    let (manager, workspace) = fixture().await;
    let root = &manager.root;
    let runs = root.path().join("runs");
    let hook = root.path().join("post-done");
    install_test_script(
        &hook,
        format!("#!/bin/sh\nprintf x >> '{}'\nexit 1\n", runs.display()),
    );
    manager
        .write_local_config(
            workspace.repository(),
            Some(format!(
                "hooks.post_done = '{}'\n[cleanup.done]\nenabled = false\n",
                hook.display()
            )),
        )
        .await
        .unwrap();
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    // After the hook failed, the same completion runs it again.
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done::default(),
            &Attacher::User,
        )
        .await
        .unwrap();
    assert_eq!(fs::read_to_string(&runs).unwrap(), "xx");
}

#[tokio::test]
async fn undone_keeps_an_unreadable_completion() {
    let (manager, workspace) = fixture().await;
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO space_completion(space_id,record,cause) VALUES (?1,'{','manual')",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(
        manager
            .spaces
            .undo_space_done(&*manager, &SpaceSelector::id(&workspace.id))
            .await
            .is_err()
    );
    assert!(manager.spaces.completion(&workspace.id).await.is_err());
}

#[test]
fn a_notice_names_the_first_results_and_counts_the_rest() {
    let results: Vec<_> = ["report.md", "repro.sh", "trace.log", "a.png", "b.png"]
        .into_iter()
        .zip(1..)
        .map(|(name, id)| HandedBack {
            id,
            name: name.into(),
        })
        .collect();
    assert_eq!(
        results_preview(&results),
        "report.md, repro.sh, trace.log and 2 more"
    );
    assert_eq!(results_preview(&results[..2]), "report.md, repro.sh");
}

#[test]
fn a_notice_selects_the_space_whose_results_it_names_by_number() {
    let completion = Completion {
        head: "head".into(),
        cleanup: false,
        integrated: false,
        results: vec![HandedBack {
            id: 1,
            name: "report.md".into(),
        }],
        summary: None,
        error: None,
    };
    let notice = done_notice("7", &completion, &[]);
    assert!(
        notice.contains("results in shoal files 7: report.md"),
        "{notice}"
    );
}

#[test]
fn a_completion_recorded_without_a_summary_reads() {
    let completion: Completion =
        serde_json::from_str(r#"{"head":"abc","cleanup":true,"integrated":false,"error":null}"#)
            .unwrap();
    assert_eq!(completion.summary, None);
}
