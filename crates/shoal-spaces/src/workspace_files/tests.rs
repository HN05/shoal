use crate::scope::Caller;
use crate::stand_in::Fixture;
use crate::workspace_files::FileSource;
use shoal_core::test_support::repository;
use shoal_daemon_base::callers::{Access, ScopedExecution};
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::assignments::Done;

use crate::model::Workspace;
use crate::workspace_files::{Attacher, KeptCopy};
use shoal_protocol::files::SentFile;
use shoal_protocol::space_names::SpaceSelector;

/// Whether permissions bind this test process; root reads and deletes past
/// them, as in CI's containers.
fn permissions_bind() -> bool {
    // SAFETY: geteuid has no arguments or memory-safety preconditions.
    (unsafe { libc::geteuid() }) != 0
}

async fn workspace(temp: &tempfile::TempDir, manager: &Fixture) -> Workspace {
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager
        .create_workspace(&repo.id, "receiver".into(), None, None, None)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_title_that_is_not_one_line_copies_nothing() {
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let workspace = workspace(temp, &manager).await;
    let file = temp.path().join("chart.html");
    std::fs::write(&file, "content").unwrap();
    let added = manager
        .spaces
        .add_attached_files(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            vec![crate::workspace_files::NewFile {
                path: file,
                title: Some("Latency\nby region".into()),
            }],
            &Attacher::User,
        )
        .await;
    assert!(added.is_err());
    assert!(
        manager
            .spaces
            .attached_files(&SpaceSelector::id(&workspace.id))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn deleting_every_workspaces_files_spares_active_ones() {
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let idle = workspace(temp, &manager).await;
    let create =
        |name: &str| manager.create_workspace(idle.repository(), name.into(), None, None, None);
    let active = create("active").await.unwrap();
    // An execution whose end the daemon could not confirm may still run.
    let unresolved = create("unresolved").await.unwrap();
    let file = temp.path().join("notes.md");
    std::fs::write(&file, "plan").unwrap();
    for workspace in [&idle, &active, &unresolved] {
        manager
            .spaces
            .add_attached_files(
                &*manager,
                &SpaceSelector::id(&workspace.id),
                vec![file.clone().into()],
                &Attacher::User,
            )
            .await
            .unwrap();
    }
    let (active_id, unresolved_id) = (active.id.clone(), unresolved.id.clone());
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO executions(id,space_id,state) VALUES ('agent',?1,'running'),
                    ('command',?2,'unknown')",
                [active_id, unresolved_id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let deleted = manager.spaces.remove_idle_files(&*manager).await.unwrap();
    let [gone] = deleted.as_slice() else {
        panic!("{deleted:?}");
    };
    assert_eq!(gone.space.name, "receiver");
    assert!(!gone.path().unwrap().exists());
    for spared in [&active, &unresolved] {
        assert_eq!(
            manager
                .spaces
                .attached_files(&SpaceSelector::id(&spared.id))
                .await
                .unwrap()
                .len(),
            1
        );
    }
}

#[tokio::test]
async fn deleting_refuses_a_copy_whose_directory_was_replaced() {
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let workspace = workspace(temp, &manager).await;
    let file = temp.path().join("notes.md");
    std::fs::write(&file, "plan").unwrap();
    let added = manager
        .spaces
        .add_attached_files(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            vec![file.into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    // The copy's directory now leads elsewhere, to a file of the same name.
    let elsewhere = temp.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("notes.md"), "keep me").unwrap();
    let directory = added[0].path().unwrap().parent().unwrap();
    std::fs::remove_dir_all(directory).unwrap();
    std::os::unix::fs::symlink(&elsewhere, directory).unwrap();
    assert!(
        manager
            .spaces
            .remove_attached_files(&*manager, &SpaceSelector::id(&workspace.id), vec![], vec![])
            .await
            .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(elsewhere.join("notes.md")).unwrap(),
        "keep me"
    );
}

#[tokio::test]
async fn expiry_refuses_results_whose_directory_was_replaced() {
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let audit = workspace(temp, &manager).await;
    let report = temp.path().join("report.md");
    std::fs::write(&report, "No leaks").unwrap();
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&audit.id),
            Done {
                results: vec![report.into()],
                ..Done::default()
            },
            &Attacher::User,
        )
        .await
        .unwrap();
    let result = manager
        .spaces
        .attached_files(&SpaceSelector::id(&audit.id))
        .await
        .unwrap()[0]
        .path()
        .unwrap()
        .to_path_buf();
    manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&audit.id),
            crate::removal::UnsavedWork::default(),
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    // The results directory now leads elsewhere, to a directory of the
    // workspace's ID.
    let results = result
        .ancestors()
        .find(|path| path.ends_with("results"))
        .unwrap()
        .to_owned();
    let elsewhere = temp.path().join("elsewhere");
    std::fs::create_dir_all(elsewhere.join(&audit.id)).unwrap();
    std::fs::write(elsewhere.join(&audit.id).join("keep.md"), "keep me").unwrap();
    std::fs::remove_dir_all(&results).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &results).unwrap();
    let id = audit.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute("DELETE FROM removed_space_files WHERE space_id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(manager.spaces.expire_results(&*manager).await.is_err());
    assert!(elsewhere.join(&audit.id).join("keep.md").exists());
}

#[tokio::test]
async fn a_completion_hands_back_a_bounded_number_of_files() {
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let audit = workspace(temp, &manager).await;
    // Each file exists, so only the count can refuse them.
    let results = (0..=shoal_protocol::files::MAX_RESULTS)
        .map(|n| {
            let path = temp.path().join(format!("{n}.md"));
            std::fs::write(&path, "result").unwrap();
            path.into()
        })
        .collect();
    let done = manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&audit.id),
            Done {
                results,
                ..Done::default()
            },
            &Attacher::User,
        )
        .await;
    assert!(done.is_err());
    assert!(
        manager
            .spaces
            .completion(&audit.id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn kept_results_answer_to_the_name_the_workspace_had_when_removed() {
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let audit = workspace(temp, &manager).await;
    let report = temp.path().join("report.md");
    std::fs::write(&report, "No leaks").unwrap();
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&audit.id),
            Done {
                results: vec![report.into()],
                ..Done::default()
            },
            &Attacher::User,
        )
        .await
        .unwrap();
    let renamed = manager
        .spaces
        .rename_branch(&*manager, &SpaceSelector::id(&audit.id), "renamed", None)
        .await
        .unwrap();
    manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&audit.id),
            crate::removal::UnsavedWork::default(),
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    let files = manager
        .spaces
        .attached_files(&SpaceSelector::id(&renamed.id))
        .await
        .unwrap();
    assert_eq!(files[0].space.name, renamed.name);
}

#[tokio::test]
async fn copying_again_refuses_a_workspace_directory_that_was_replaced() {
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let workspace = workspace(temp, &manager).await;
    let (notes, shot) = (temp.path().join("notes.md"), temp.path().join("shot.png"));
    std::fs::write(&notes, "plan").unwrap();
    std::fs::write(&shot, "image").unwrap();
    let files = vec![notes.into(), shot.into()];
    let added = manager
        .spaces
        .add_attached_files(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            files.clone(),
            &Attacher::User,
        )
        .await
        .unwrap();
    // The workspace's directory now leads elsewhere, where a copy is gone.
    let directory = added[0]
        .path()
        .unwrap()
        .ancestors()
        .nth(3)
        .unwrap()
        .to_owned();
    let elsewhere = temp.path().join("elsewhere");
    std::fs::rename(&directory, &elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &directory).unwrap();
    let inside = added[1].path().unwrap().strip_prefix(&directory).unwrap();
    std::fs::remove_file(
        elsewhere.join(added[0].path().unwrap().strip_prefix(&directory).unwrap()),
    )
    .unwrap();
    assert!(
        manager
            .spaces
            .add_attached_files(
                &*manager,
                &SpaceSelector::id(&workspace.id),
                files,
                &Attacher::User
            )
            .await
            .is_err()
    );
    assert!(elsewhere.join(inside).exists());
}

#[tokio::test]
async fn sent_files_relayed_back_take_their_copies_paths_by_id() {
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let workspace = workspace(temp, &manager).await;
    let file = temp.path().join("notes.md");
    std::fs::write(&file, "plan").unwrap();
    let added = manager
        .spaces
        .add_attached_files(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            vec![file.into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    // The records as another machine read them, and one removed since.
    let elsewhere = |id: i64| SentFile {
        id,
        space: workspace.id.clone(),
        name: "notes.md".into(),
        title: None,
        copy: KeptCopy::Elsewhere {
            machine: "here".into(),
        },
    };
    let kept = manager
        .spaces
        .kept_sent_files(vec![elsewhere(added[0].id + 1), elsewhere(added[0].id)])
        .await
        .unwrap();
    let paths: Vec<_> = kept.iter().map(SentFile::path).collect();
    assert_eq!(paths, [added[0].path()]);
}

#[tokio::test]
async fn handing_back_a_changed_file_completes_again() {
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let audit = workspace(temp, &manager).await;
    let report = temp.path().join("report.md");
    std::fs::write(&report, "First draft").unwrap();
    let first = manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&audit.id),
            Done {
                results: vec![report.clone().into()],
                ..Done::default()
            },
            &Attacher::User,
        )
        .await
        .unwrap();
    // The same file again changes nothing.
    let again = manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&audit.id),
            Done {
                results: vec![report.clone().into()],
                ..Done::default()
            },
            &Attacher::User,
        )
        .await;
    assert!(again.is_err());
    std::fs::write(&report, "Revised").unwrap();
    let revised = manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&audit.id),
            Done {
                results: vec![report.into()],
                ..Done::default()
            },
            &Attacher::User,
        )
        .await
        .unwrap();
    assert_eq!(revised.results[0].name, first.results[0].name);
    assert_ne!(revised.results[0].id, first.results[0].id);
}

#[tokio::test]
async fn files_that_fail_to_go_with_their_repository_stay_recorded() {
    use std::os::unix::fs::PermissionsExt;
    if !permissions_bind() {
        return;
    }
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let workspace = workspace(temp, &manager).await;
    let file = temp.path().join("notes.md");
    std::fs::write(&file, "plan").unwrap();
    let added = manager
        .spaces
        .add_attached_files(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            vec![file.into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    // The copy's directory refuses to give it up.
    let directory = added[0].path().unwrap().parent().unwrap().to_owned();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o500)).unwrap();
    let repo = manager
        .find_repository(workspace.repository())
        .await
        .unwrap();
    let deleted = manager.spaces.delete_repository_files(&repo).await;
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(deleted.is_err(), "the copy could not be deleted");
    assert!(added[0].path().unwrap().exists());
    assert_eq!(
        manager
            .spaces
            .attached_files(&SpaceSelector::id(&workspace.id))
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn a_disposable_workspaces_files_outlive_it_for_the_retention_period() {
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let receiver = workspace(temp, &manager).await;
    let disposable = manager
        .spaces
        .create_workspace_with(
            &*manager,
            receiver.repository(),
            "scratch".into(),
            None,
            crate::workspace::NewWorkspace {
                options: crate::protocol::CreationOptions {
                    disposable: true,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let file = temp.path().join("notes.md");
    std::fs::write(&file, "notes").unwrap();
    let added = manager
        .spaces
        .add_attached_files(
            &*manager,
            &SpaceSelector::id(&disposable.id),
            vec![file.into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    let [added] = added.as_slice() else {
        panic!("{added:?}");
    };
    manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&disposable.id),
            crate::removal::UnsavedWork::default(),
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    manager.spaces.expire_results(&*manager).await.unwrap();
    let kept = manager
        .spaces
        .attached_files(&SpaceSelector::id(&disposable.id))
        .await
        .unwrap();
    let [kept] = kept.as_slice() else {
        panic!("{kept:?}");
    };
    assert_eq!(kept.path().unwrap(), added.path().unwrap());
    assert_eq!(kept.space.name, "scratch");
    assert!(kept.expires_at.is_some());
    assert!(added.path().unwrap().exists());
}

#[tokio::test]
async fn removal_deletes_the_directories_of_files_already_deleted() {
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let workspace = workspace(temp, &manager).await;
    let file = temp.path().join("notes.md");
    std::fs::write(&file, "notes").unwrap();
    let added = manager
        .spaces
        .add_attached_files(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            vec![file.into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    let [added] = added.as_slice() else {
        panic!("{added:?}");
    };
    manager
        .spaces
        .remove_attached_files(&*manager, &SpaceSelector::id(&workspace.id), vec![], vec![])
        .await
        .unwrap();
    let directory = added.path().unwrap().ancestors().nth(3).unwrap().to_owned();
    assert!(directory.ends_with(workspace.id.as_str()));
    assert!(!directory.exists(), "deleting its last copy empties it");
    // A leftover no record names, as a failed discard leaves.
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("leftover"), "").unwrap();
    manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            crate::removal::UnsavedWork::default(),
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert!(!directory.exists());
}

#[tokio::test]
async fn files_from_outside_any_agent_are_the_users() {
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let workspace = workspace(temp, &manager).await;
    assert_eq!(
        manager.spaces.attacher(&*manager, None).await,
        Attacher::User
    );
    // A workspace terminal or a command, scoped without an agent.
    let terminal = Caller {
        space_id: workspace.id.clone(),
        execution: None,
        granted: None,
        access: Access::Full,
        vouched: None,
    };
    assert_eq!(
        manager.spaces.attacher(&*manager, Some(&terminal)).await,
        Attacher::User
    );
    let command = Caller {
        execution: Some(ScopedExecution {
            id: ExecutionId::generate(),
            kind: shoal_protocol::executions::ExecutionKind::Command,
            agent_execution: None,
        }),
        ..terminal.clone()
    };
    assert_eq!(
        manager.spaces.attacher(&*manager, Some(&command)).await,
        Attacher::User
    );

    let file = temp.path().join("notes.md");
    std::fs::write(&file, "plan").unwrap();
    let attacher = manager.spaces.attacher(&*manager, None).await;
    let [added] = manager
        .spaces
        .add_attached_files(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            vec![file.into()],
            &attacher,
        )
        .await
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(added.attached_by, Attacher::User);
    assert_eq!(
        serde_json::to_value(&added).unwrap()["attached_by"],
        serde_json::json!({"kind": "user"})
    );
}

#[tokio::test]
async fn results_name_the_agent_that_handed_them_back() {
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let workspace = workspace(temp, &manager).await;
    let agent = manager.run_named_agent(&workspace.id, "codex").await;
    let caller = Caller {
        execution: Some(ScopedExecution {
            id: agent,
            kind: shoal_protocol::executions::ExecutionKind::Command,
            agent_execution: None,
        }),
        ..Caller::of_space(workspace.id.clone())
    };
    let attacher = manager.spaces.attacher(&*manager, Some(&caller)).await;
    let report = temp.path().join("report.md");
    std::fs::write(&report, "done").unwrap();
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Done {
                results: vec![report.into()],
                ..Done::default()
            },
            &attacher,
        )
        .await
        .unwrap();
    let files = manager
        .spaces
        .attached_files(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    let [result] = files.as_slice() else {
        panic!("{files:?}");
    };
    assert_eq!(result.source, FileSource::Result);
    assert_eq!(
        result.attached_by,
        Attacher::Agent {
            agent: "codex".into(),
            execution: caller.execution_id().unwrap().clone(),
            session: None,
        }
    );
}
