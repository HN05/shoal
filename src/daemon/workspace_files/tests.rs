use crate::client::assignments::Done;
use crate::client::execution_ids::ExecutionId;
use std::time::Duration;

use super::{Attacher, FileSource};
use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::{
        scope::Caller,
        test_support::{manager, repository},
        workspace::{ExecutionKind, Manager},
    },
    execution::recovery,
    model::Workspace,
};

/// Whether permissions bind this test process; root reads and deletes past
/// them, as in CI's containers.
fn permissions_bind() -> bool {
    // SAFETY: geteuid has no arguments or memory-safety preconditions.
    (unsafe { libc::geteuid() }) != 0
}

async fn workspace(temp: &tempfile::TempDir, manager: &Manager) -> Workspace {
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

/// The caller that the processes of a command registered by `caller` in
/// `workspace` send requests as, with the command's execution.
async fn command_caller(
    manager: &Manager,
    workspace: &Workspace,
    caller: Option<&Caller>,
) -> Caller {
    let token = manager
        .begin_execution(workspace.id.as_str(), None, ExecutionKind::Command, caller)
        .await
        .unwrap()
        .plan
        .scope_token;
    manager.caller(&token).await.unwrap().unwrap()
}

/// The caller that the processes of a tracked agent `name` in `workspace`
/// send requests as.
async fn agent_caller(manager: &Manager, workspace: &Workspace, name: &str) -> Caller {
    let caller = command_caller(manager, workspace, None).await;
    manager
        .track_agent(
            caller.execution_id().unwrap(),
            &crate::execution::TrackedAgent::named(name),
            &workspace.id,
            &workspace.name,
            true,
            Duration::ZERO,
        )
        .await;
    caller
}

#[tokio::test]
async fn files_from_outside_any_agent_are_the_users() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager).await;
    assert_eq!(manager.attacher(None, None).await, Attacher::User);
    // A workspace terminal or a command, scoped without an agent.
    let terminal = Caller {
        space_id: workspace.id.clone(),
        execution: None,
    };
    assert_eq!(
        manager.attacher(Some(&terminal), None).await,
        Attacher::User
    );
    let command = command_caller(&manager, &workspace, None).await;
    assert_eq!(manager.attacher(Some(&command), None).await, Attacher::User);

    let file = temp.path().join("notes.md");
    std::fs::write(&file, "plan").unwrap();
    let attacher = manager.attacher(None, None).await;
    let [added] = manager
        .add_attached_files(
            &SpaceSelector::from(&workspace.name),
            vec![file.into()],
            &attacher,
        )
        .await
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(added.attached_by, Some(Attacher::User));
    assert_eq!(
        serde_json::to_value(&added).unwrap()["attached_by"],
        serde_json::json!({"kind": "user"})
    );
}

#[tokio::test]
async fn an_agents_files_name_its_kind_and_session() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager).await;
    let caller = agent_caller(&manager, &workspace, "happy claude").await;
    let execution = caller.execution_id().unwrap().to_owned();
    // An agent that has not reported its session yet.
    assert_eq!(
        manager.attacher(Some(&caller), None).await,
        Attacher::Agent {
            agent: "happy-claude".into(),
            execution: execution.clone(),
            session: None,
        }
    );
    recovery::save_session(&manager.paths, &workspace.id, &execution, "s1").unwrap();
    let attacher = manager.attacher(Some(&caller), None).await;
    assert_eq!(
        serde_json::to_value(&attacher).unwrap(),
        serde_json::json!({
            "kind": "agent",
            "agent": "happy-claude",
            "execution": execution,
            "session": "s1",
        })
    );

    let (notes, log) = (temp.path().join("notes.md"), temp.path().join("crash.log"));
    std::fs::write(&notes, "plan").unwrap();
    std::fs::write(&log, "panic").unwrap();
    manager
        .add_attached_files(
            &SpaceSelector::from(&workspace.name),
            vec![notes.into()],
            &attacher,
        )
        .await
        .unwrap();
    manager
        .send_agent_message(
            &SpaceSelector::from(&workspace.name),
            "See the log".into(),
            vec![log.into()],
            &attacher,
        )
        .await
        .unwrap();
    // A command the agent runs with `shoal run`, and one that runs.
    let command = command_caller(&manager, &workspace, Some(&caller)).await;
    let nested = command_caller(&manager, &workspace, Some(&command)).await;
    assert_eq!(manager.attacher(Some(&nested), None).await, attacher);
    // A reattaching command keeps the agent it runs under.
    let id = nested.execution_id().unwrap().to_owned();
    let recorded: Option<ExecutionId> = manager
        .store
        .run(move |db| {
            Ok(db.query_row(
                "SELECT agent_execution FROM executions WHERE id=?1",
                [id],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(recorded, Some(execution.clone()));
    // A command reattaching after a restart before its agent, and one it
    // runs then.
    manager
        .executions
        .with_agents(|agents| agents.remove(&execution))
        .await
        .unwrap();
    assert_eq!(manager.attacher(Some(&nested), None).await, attacher);
    let later = command_caller(&manager, &workspace, Some(&nested)).await;
    assert_eq!(manager.attacher(Some(&later), None).await, attacher);
    let files = manager
        .attached_files(&SpaceSelector::from(&workspace.name))
        .await
        .unwrap();
    assert_eq!(files.len(), 2, "{files:?}");
    assert!(
        files
            .iter()
            .all(|file| file.attached_by.as_ref() == Some(&attacher)),
        "{files:?}"
    );
}

#[tokio::test]
async fn results_name_the_agent_that_handed_them_back() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager).await;
    let caller = agent_caller(&manager, &workspace, "codex").await;
    let attacher = manager.attacher(Some(&caller), None).await;
    let report = temp.path().join("report.md");
    std::fs::write(&report, "done").unwrap();
    manager
        .mark_done(
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
        .attached_files(&SpaceSelector::from(&workspace.name))
        .await
        .unwrap();
    let [result] = files.as_slice() else {
        panic!("{files:?}");
    };
    assert_eq!(result.source, FileSource::Result);
    assert_eq!(
        result.attached_by,
        Some(Attacher::Agent {
            agent: "codex".into(),
            execution: caller.execution_id().unwrap().clone(),
            session: None,
        })
    );
}

#[tokio::test]
async fn files_sent_with_messages_are_listed_until_removal() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager).await;
    let file = temp.path().join("crash.log");
    std::fs::write(&file, "panic").unwrap();
    assert!(
        manager
            .attached_files(&SpaceSelector::from(&workspace.name))
            .await
            .unwrap()
            .is_empty()
    );
    // Sending the same file again keeps one record of its copy.
    for _ in 0..2 {
        manager
            .send_agent_message(
                &SpaceSelector::from(&workspace.name),
                "See the log".into(),
                vec![file.clone().into()],
                &Attacher::User,
            )
            .await
            .unwrap();
    }
    let files = manager
        .attached_files(&SpaceSelector::from(&workspace.name))
        .await
        .unwrap();
    let [listed] = files.as_slice() else {
        panic!("{files:?}");
    };
    assert_eq!(listed.name, "crash.log");
    assert_eq!(listed.bytes, 5);
    assert_eq!(listed.source, FileSource::Message);
    assert_eq!(listed.workspace.as_deref(), Some("receiver"));
    assert_eq!(std::fs::read_to_string(&listed.path).unwrap(), "panic");

    manager
        .remove_workspace(
            &SpaceSelector::id(&workspace.id),
            crate::removal::UnsavedWork::default(),
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert!(!listed.path.exists());
    let left = manager
        .store
        .run(|db| {
            Ok(
                db.query_row("SELECT COUNT(*) FROM workspace_files", [], |row| {
                    row.get::<_, i64>(0)
                })?,
            )
        })
        .await
        .unwrap();
    assert_eq!(left, 0);
}

#[tokio::test]
async fn added_files_need_no_message() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager).await;
    let file = temp.path().join("config.toml");
    std::fs::write(&file, "port = 1").unwrap();
    let added = manager
        .add_attached_files(
            &SpaceSelector::from(&workspace.name),
            vec![file.clone().into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    let [copy] = added.as_slice() else {
        panic!("{added:?}");
    };
    assert_eq!(copy.source, FileSource::Added);
    assert_eq!(std::fs::read_to_string(&copy.path).unwrap(), "port = 1");
    // Adding it again keeps the copy; sending it is a separate copy.
    manager
        .add_attached_files(
            &SpaceSelector::from(&workspace.name),
            vec![file.clone().into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    manager
        .send_agent_message(
            &SpaceSelector::from(&workspace.name),
            String::new(),
            vec![file.into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    let files = manager
        .attached_files(&SpaceSelector::from(&workspace.name))
        .await
        .unwrap();
    let sources: Vec<_> = files.iter().map(|file| file.source).collect();
    assert_eq!(sources, [FileSource::Added, FileSource::Message]);
    assert!(
        manager
            .agent_messages(&SpaceSelector::from(&workspace.name))
            .await
            .unwrap()
            .iter()
            .all(|message| message.attachments.len() == 1),
        "adding sends no message"
    );
}

#[tokio::test]
async fn titles_stay_with_their_files_until_a_new_one() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager).await;
    let (chart, shot, report) = (
        temp.path().join("chart.html"),
        temp.path().join("shot.png"),
        temp.path().join("report.md"),
    );
    for file in [&chart, &shot, &report] {
        std::fs::write(file, "content").unwrap();
    }
    let titled = |path: &std::path::Path, title: &str| super::NewFile {
        path: path.to_owned(),
        title: Some(title.into()),
    };
    manager
        .add_attached_files(
            &SpaceSelector::from(&workspace.name),
            vec![titled(&chart, "Latency by region")],
            &Attacher::User,
        )
        .await
        .unwrap();
    // Adding it again without a title keeps the title.
    manager
        .add_attached_files(
            &SpaceSelector::from(&workspace.name),
            vec![chart.clone().into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    manager
        .send_agent_message(
            &SpaceSelector::from(&workspace.name),
            String::new(),
            vec![titled(&shot, "Login screen"), report.clone().into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    manager
        .mark_done(
            &SpaceSelector::id(&workspace.id),
            Done {
                results: vec![titled(&report, "Audit findings")],
                ..Done::default()
            },
            &Attacher::User,
        )
        .await
        .unwrap();
    let titles = |files: Vec<super::AttachedFile>| -> Vec<_> {
        files
            .into_iter()
            .map(|file| (file.name, file.title))
            .collect()
    };
    let listed = manager
        .attached_files(&SpaceSelector::from(&workspace.name))
        .await
        .unwrap();
    assert_eq!(
        titles(listed),
        [
            ("chart.html".into(), Some("Latency by region".into())),
            ("shot.png".into(), Some("Login screen".into())),
            ("report.md".into(), None),
            ("report.md".into(), Some("Audit findings".into())),
        ]
    );
    // A new title replaces the old one.
    let added = manager
        .add_attached_files(
            &SpaceSelector::from(&workspace.name),
            vec![titled(&chart, "Latency")],
            &Attacher::User,
        )
        .await
        .unwrap();
    assert_eq!(
        titles(added),
        [("chart.html".into(), Some("Latency".into()))]
    );
}

#[tokio::test]
async fn a_title_that_is_not_one_line_copies_nothing() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager).await;
    let file = temp.path().join("chart.html");
    std::fs::write(&file, "content").unwrap();
    let added = manager
        .add_attached_files(
            &SpaceSelector::from(&workspace.name),
            vec![super::NewFile {
                path: file,
                title: Some("Latency\nby region".into()),
            }],
            &Attacher::User,
        )
        .await;
    assert!(added.is_err());
    assert!(
        manager
            .attached_files(&SpaceSelector::from(&workspace.name))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn deleting_by_name_keeps_the_other_files() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager).await;
    let (log, shot) = (temp.path().join("crash.log"), temp.path().join("shot.png"));
    std::fs::write(&log, "panic").unwrap();
    std::fs::write(&shot, "image").unwrap();
    manager
        .send_agent_message(
            &SpaceSelector::from(&workspace.name),
            "See".into(),
            vec![log.clone().into(), shot.into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    assert!(
        manager
            .remove_attached_files(
                &SpaceSelector::from(&workspace.name),
                vec!["missing.txt".into()],
                vec![]
            )
            .await
            .is_err()
    );
    let deleted = manager
        .remove_attached_files(
            &SpaceSelector::from(&workspace.name),
            vec!["crash.log".into()],
            vec![],
        )
        .await
        .unwrap();
    assert!(!deleted[0].path.exists());
    let left = manager
        .attached_files(&SpaceSelector::from(&workspace.name))
        .await
        .unwrap();
    let [kept] = left.as_slice() else {
        panic!("{left:?}");
    };
    assert_eq!(kept.name, "shot.png");
    assert!(kept.path.exists());

    // Sending the same files again copies the deleted one anew.
    manager
        .send_agent_message(
            &SpaceSelector::from(&workspace.name),
            "See".into(),
            vec![log.into(), temp.path().join("shot.png").into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    assert!(deleted[0].path.exists());
    assert_eq!(
        manager
            .attached_files(&SpaceSelector::from(&workspace.name))
            .await
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn deleting_every_workspaces_files_spares_active_ones() {
    let (temp, manager) = manager().await;
    let idle = workspace(&temp, &manager).await;
    let create =
        |name: &str| manager.create_workspace(&idle.repository_id, name.into(), None, None, None);
    let active = create("active").await.unwrap();
    // An execution whose end the daemon could not confirm may still run.
    let unresolved = create("unresolved").await.unwrap();
    let file = temp.path().join("notes.md");
    std::fs::write(&file, "plan").unwrap();
    for workspace in [&idle, &active, &unresolved] {
        manager
            .add_attached_files(
                &SpaceSelector::from(&workspace.name),
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
                "INSERT INTO executions(id,workspace_id,state) VALUES ('agent',?1,'running'),
                    ('command',?2,'unknown')",
                [active_id, unresolved_id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let deleted = manager.remove_idle_files().await.unwrap();
    let [gone] = deleted.as_slice() else {
        panic!("{deleted:?}");
    };
    assert_eq!(gone.workspace.as_deref(), Some("receiver"));
    assert!(!gone.path.exists());
    for spared in [&active, &unresolved] {
        assert_eq!(
            manager
                .attached_files(&SpaceSelector::from(&spared.name))
                .await
                .unwrap()
                .len(),
            1
        );
    }
}

#[tokio::test]
async fn results_outlive_removal_as_long_as_its_record() {
    use crate::daemon::{
        notifications::NotificationKind,
        scope::{self, Caller},
    };
    use crate::protocol::FileMethod;
    let (temp, manager) = manager().await;
    let parent = workspace(&temp, &manager).await;
    let child = manager
        .split_workspace(
            &SpaceSelector::id(&parent.id),
            "audit".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let report = temp.path().join("report.md");
    std::fs::write(&report, "No leaks").unwrap();
    let completion = manager
        .mark_done(
            &SpaceSelector::id(&child.id),
            Done {
                results: vec![report.into()],
                ..Done::default()
            },
            &Attacher::User,
        )
        .await
        .unwrap();
    assert_eq!(completion.results[0].name, "report.md");
    let result = manager
        .attached_files(&SpaceSelector::id(&child.id))
        .await
        .unwrap()[0]
        .path
        .clone();
    assert_eq!(std::fs::read_to_string(&result).unwrap(), "No leaks");
    assert!(
        manager
            .notifications(true, 50)
            .await
            .unwrap()
            .iter()
            .any(|n| n.kind == NotificationKind::SpaceDone && n.message.contains("report.md")),
    );

    manager
        .remove_workspace(
            &SpaceSelector::id(&child.id),
            crate::removal::UnsavedWork::default(),
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    manager.expire_results().await.unwrap();
    let files = manager.attached_files(&"audit".into()).await.unwrap();
    let [kept] = files.as_slice() else {
        panic!("{files:?}");
    };
    assert_eq!(kept.source, FileSource::Result);
    assert!(kept.expires_at.is_some(), "{kept:?}");
    assert!(result.exists());

    // The parent's processes reach the removed child's results; others do not.
    let sibling = manager
        .create_workspace(&parent.repository_id, "sibling".into(), None, None, None)
        .await
        .unwrap();
    for (token, workspace, allowed) in [("parent", &parent, true), ("sibling", &sibling, false)] {
        manager
            .executions
            .issue_scope(
                token.into(),
                Caller {
                    space_id: workspace.id.clone(),
                    execution: None,
                },
            )
            .await;
        let mut list = crate::protocol::Operation::from(FileMethod::ListFiles {
            space: "audit".into(),
        });
        let authorized = scope::authorize(&manager, Some(&token.into()), &mut list).await;
        assert_eq!(authorized.is_ok(), allowed, "{token}");
    }

    let id = child.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute("DELETE FROM removed_workspaces WHERE workspace_id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();
    manager.expire_results().await.unwrap();
    assert!(!result.exists());
    assert!(manager.attached_files(&"audit".into()).await.is_err());
}

#[tokio::test]
async fn results_stay_reachable_when_a_workspace_between_goes() {
    use crate::daemon::scope::{self, Caller};
    use crate::protocol::FileMethod;
    let (temp, manager) = manager().await;
    let top = workspace(&temp, &manager).await;
    let middle = manager
        .split_workspace(
            &SpaceSelector::id(&top.id),
            "middle".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let bottom = manager
        .split_workspace(
            &SpaceSelector::id(&middle.id),
            "bottom".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let report = temp.path().join("report.md");
    std::fs::write(&report, "No leaks").unwrap();
    manager
        .mark_done(
            &SpaceSelector::id(&bottom.id),
            Done {
                results: vec![report.into()],
                ..Done::default()
            },
            &Attacher::User,
        )
        .await
        .unwrap();
    for removed in [&bottom, &middle] {
        manager
            .remove_workspace(
                &SpaceSelector::id(&removed.id),
                crate::removal::UnsavedWork::default(),
                crate::removal::InspectionPolicy::GitOnly,
            )
            .await
            .unwrap();
    }
    manager
        .executions
        .issue_scope(
            "top".into(),
            Caller {
                space_id: top.id.clone(),
                execution: None,
            },
        )
        .await;
    let mut list = crate::protocol::Operation::from(FileMethod::ListFiles {
        space: "bottom".into(),
    });
    scope::authorize(&manager, Some(&"top".into()), &mut list)
        .await
        .unwrap();
}

#[tokio::test]
async fn deleting_refuses_a_copy_whose_directory_was_replaced() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager).await;
    let file = temp.path().join("notes.md");
    std::fs::write(&file, "plan").unwrap();
    let added = manager
        .add_attached_files(
            &SpaceSelector::from(&workspace.name),
            vec![file.into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    // The copy's directory now leads elsewhere, to a file of the same name.
    let elsewhere = temp.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("notes.md"), "keep me").unwrap();
    let directory = added[0].path.parent().unwrap();
    std::fs::remove_dir_all(directory).unwrap();
    std::os::unix::fs::symlink(&elsewhere, directory).unwrap();
    assert!(
        manager
            .remove_attached_files(&SpaceSelector::from(&workspace.name), vec![], vec![])
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
    let (temp, manager) = manager().await;
    let audit = workspace(&temp, &manager).await;
    let report = temp.path().join("report.md");
    std::fs::write(&report, "No leaks").unwrap();
    manager
        .mark_done(
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
        .attached_files(&SpaceSelector::id(&audit.id))
        .await
        .unwrap()[0]
        .path
        .clone();
    manager
        .remove_workspace(
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
            db.execute("DELETE FROM removed_workspaces WHERE workspace_id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(manager.expire_results().await.is_err());
    assert!(elsewhere.join(&audit.id).join("keep.md").exists());
}

#[tokio::test]
async fn a_completion_hands_back_a_bounded_number_of_files() {
    let (temp, manager) = manager().await;
    let audit = workspace(&temp, &manager).await;
    // Each file exists, so only the count can refuse them.
    let results = (0..=crate::client::files::MAX_RESULTS)
        .map(|n| {
            let path = temp.path().join(format!("{n}.md"));
            std::fs::write(&path, "result").unwrap();
            path.into()
        })
        .collect();
    let done = manager
        .mark_done(
            &SpaceSelector::id(&audit.id),
            Done {
                results,
                ..Done::default()
            },
            &Attacher::User,
        )
        .await;
    assert!(done.is_err());
    assert!(manager.completion(&audit.id).await.unwrap().is_none());
}

#[tokio::test]
async fn kept_results_answer_to_the_name_the_workspace_had_when_removed() {
    let (temp, manager) = manager().await;
    let audit = workspace(&temp, &manager).await;
    let report = temp.path().join("report.md");
    std::fs::write(&report, "No leaks").unwrap();
    manager
        .mark_done(
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
        .rename_workspace(&SpaceSelector::id(&audit.id), "renamed", None)
        .await
        .unwrap();
    manager
        .remove_workspace(
            &SpaceSelector::id(&audit.id),
            crate::removal::UnsavedWork::default(),
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    let files = manager
        .attached_files(&SpaceSelector::from(&renamed.name))
        .await
        .unwrap();
    assert_eq!(files[0].workspace, Some(renamed.name.to_string()));
}

#[tokio::test]
async fn copying_again_refuses_a_workspace_directory_that_was_replaced() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager).await;
    let (notes, shot) = (temp.path().join("notes.md"), temp.path().join("shot.png"));
    std::fs::write(&notes, "plan").unwrap();
    std::fs::write(&shot, "image").unwrap();
    let files = vec![notes.into(), shot.into()];
    let added = manager
        .add_attached_files(
            &SpaceSelector::from(&workspace.name),
            files.clone(),
            &Attacher::User,
        )
        .await
        .unwrap();
    // The workspace's directory now leads elsewhere, where a copy is gone.
    let directory = added[0].path.ancestors().nth(3).unwrap().to_owned();
    let elsewhere = temp.path().join("elsewhere");
    std::fs::rename(&directory, &elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &directory).unwrap();
    let inside = added[1].path.strip_prefix(&directory).unwrap();
    std::fs::remove_file(elsewhere.join(added[0].path.strip_prefix(&directory).unwrap())).unwrap();
    assert!(
        manager
            .add_attached_files(
                &SpaceSelector::from(&workspace.name),
                files,
                &Attacher::User
            )
            .await
            .is_err()
    );
    assert!(elsewhere.join(inside).exists());
}

#[tokio::test]
async fn deleting_a_record_keeps_other_copies_of_the_same_name() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager).await;
    let file = temp.path().join("notes.md");
    std::fs::write(&file, "plan").unwrap();
    let added = manager
        .add_attached_files(
            &SpaceSelector::from(&workspace.name),
            vec![file.clone().into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    manager
        .send_agent_message(
            &SpaceSelector::from(&workspace.name),
            String::new(),
            vec![file.into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    let deleted = manager
        .remove_attached_files(
            &SpaceSelector::from(&workspace.name),
            vec![],
            vec![added[0].id],
        )
        .await
        .unwrap();
    assert_eq!(deleted, added);
    let left = manager
        .attached_files(&SpaceSelector::from(&workspace.name))
        .await
        .unwrap();
    let [sent] = left.as_slice() else {
        panic!("{left:?}");
    };
    assert_eq!(sent.source, FileSource::Message);
    assert!(sent.path.exists());
}

#[tokio::test]
async fn handing_back_a_changed_file_completes_again() {
    let (temp, manager) = manager().await;
    let audit = workspace(&temp, &manager).await;
    let report = temp.path().join("report.md");
    std::fs::write(&report, "First draft").unwrap();
    let first = manager
        .mark_done(
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
        .mark_done(
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
        .mark_done(
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
async fn upgrading_records_the_copies_messages_attached_before() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager).await;
    let (log, env) = (temp.path().join("crash.log"), temp.path().join(".env"));
    std::fs::write(&log, "panic").unwrap();
    std::fs::write(&env, "PORT=1").unwrap();
    manager
        .send_agent_message(
            &SpaceSelector::from(&workspace.name),
            "See".into(),
            vec![log.into(), env.into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    let copies: Vec<_> = manager
        .attached_files(&SpaceSelector::from(&workspace.name))
        .await
        .unwrap()
        .into_iter()
        .map(|file| file.path)
        .collect();
    // A state database from before Shoal recorded attached files.
    manager
        .store
        .run(|db| {
            db.execute_batch(include_str!("../store/space_events_undo.sql"))?;
            db.execute_batch(BEFORE_ATTACHED_FILES)?;
            Ok(())
        })
        .await
        .unwrap();
    let reopened = Manager::open(manager.paths.clone()).await.unwrap();
    // A move of the repository's directory is unfinished until recovery.
    let repository_id = workspace.repository_id.clone();
    reopened
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO repository_moves
                    (repository_id,source,destination,source_id,destination_id,states)
                    VALUES (?1,'/old','/new','old','new','{}')",
                [repository_id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    reopened.record_earlier_files().await.unwrap();
    assert!(
        reopened
            .attached_files(&SpaceSelector::from(&workspace.name))
            .await
            .unwrap()
            .is_empty()
    );
    reopened
        .store
        .run(|db| {
            db.execute("DELETE FROM repository_moves", [])?;
            Ok(())
        })
        .await
        .unwrap();
    reopened.record_earlier_files().await.unwrap();
    let files = reopened
        .attached_files(&SpaceSelector::from(&workspace.name))
        .await
        .unwrap();
    let mut recorded: Vec<_> = files.iter().map(|file| file.path.clone()).collect();
    recorded.sort();
    let mut expected = copies;
    expected.sort();
    assert_eq!(recorded, expected, "a hidden file name is a copy too");
    assert!(
        files
            .iter()
            .all(|file| file.source == FileSource::Message && file.attached_by.is_none()),
        "{files:?}"
    );
    reopened.store.shutdown().await;
}

#[tokio::test]
async fn files_that_fail_to_go_with_their_repository_stay_recorded() {
    use std::os::unix::fs::PermissionsExt;
    if !permissions_bind() {
        return;
    }
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager).await;
    let file = temp.path().join("notes.md");
    std::fs::write(&file, "plan").unwrap();
    let added = manager
        .add_attached_files(
            &SpaceSelector::from(&workspace.name),
            vec![file.into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    // The copy's directory refuses to give it up.
    let directory = added[0].path.parent().unwrap().to_owned();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o500)).unwrap();
    let repo = manager.repository(&workspace.repository_id).await.unwrap();
    let deleted = manager.delete_repository_files(&repo).await;
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(deleted.is_err(), "the copy could not be deleted");
    assert!(added[0].path.exists());
    assert_eq!(
        manager
            .attached_files(&SpaceSelector::from(&workspace.name))
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn an_earlier_copy_that_cannot_be_read_is_recorded_on_a_later_pass() {
    use std::os::unix::fs::PermissionsExt;
    if !permissions_bind() {
        return;
    }
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager).await;
    let file = temp.path().join("crash.log");
    std::fs::write(&file, "panic").unwrap();
    manager
        .send_agent_message(
            &SpaceSelector::from(&workspace.name),
            "See".into(),
            vec![file.into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    let copy = manager
        .attached_files(&SpaceSelector::from(&workspace.name))
        .await
        .unwrap()[0]
        .path
        .clone();
    manager
        .store
        .run(|db| {
            db.execute_batch(include_str!("../store/space_events_undo.sql"))?;
            db.execute_batch(BEFORE_ATTACHED_FILES)?;
            Ok(())
        })
        .await
        .unwrap();
    let reopened = Manager::open(manager.paths.clone()).await.unwrap();
    // The message's directory cannot be read for now.
    let message = copy.ancestors().nth(2).unwrap().to_owned();
    std::fs::set_permissions(&message, std::fs::Permissions::from_mode(0o000)).unwrap();
    let failed = reopened.record_earlier_files().await;
    std::fs::set_permissions(&message, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(failed.is_err(), "the unreadable directory fails the pass");
    reopened.record_earlier_files().await.unwrap();
    let files = reopened
        .attached_files(&SpaceSelector::from(&workspace.name))
        .await
        .unwrap();
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(files[0].path, copy);
    reopened.store.shutdown().await;
}

#[tokio::test]
async fn an_unreadable_owner_marker_keeps_the_upgrade_mark() {
    use std::os::unix::fs::PermissionsExt;
    if !permissions_bind() {
        return;
    }
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager).await;
    let file = temp.path().join("crash.log");
    std::fs::write(&file, "panic").unwrap();
    manager
        .send_agent_message(
            &SpaceSelector::from(&workspace.name),
            "See".into(),
            vec![file.into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    manager
        .store
        .run(|db| {
            db.execute_batch(include_str!("../store/space_events_undo.sql"))?;
            db.execute_batch(BEFORE_ATTACHED_FILES)?;
            Ok(())
        })
        .await
        .unwrap();
    let reopened = Manager::open(manager.paths.clone()).await.unwrap();
    let repo = reopened.repository(&workspace.repository_id).await.unwrap();
    let marker = repo
        .workspaces_dir
        .unwrap()
        .join(".attachments")
        .join(crate::fsutil::marked_dirs::OWNER_MARKER);
    std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o000)).unwrap();
    let failed = reopened.record_earlier_files().await;
    std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(failed.is_err(), "an unreadable marker fails the pass");
    reopened.record_earlier_files().await.unwrap();
    assert_eq!(
        reopened
            .attached_files(&SpaceSelector::from(&workspace.name))
            .await
            .unwrap()
            .len(),
        1
    );
    reopened.store.shutdown().await;
}

/// Return a current state database to version 87, before Shoal recorded
/// attached files, undoing the later migrations that cannot run twice.
const BEFORE_ATTACHED_FILES: &str = "DROP TRIGGER workspace_progress_removed;
    DROP TRIGGER chat_progress_removed;
    DROP TABLE space_progress;
    ALTER TABLE chats DROP COLUMN root_dir;
    ALTER TABLE workspaces DROP COLUMN copied_from;
    ALTER TABLE workspaces DROP COLUMN copied_from_checkpoint;
    ALTER TABLE workspaces DROP COLUMN starting_checkpoint;
    ALTER TABLE workspaces DROP COLUMN starting_index;
    DROP TABLE merges;
    DROP TRIGGER chat_agent_state_ended;
    DROP TRIGGER chat_agent_state_removed;
    DROP TRIGGER workspace_agent_state_removed;
    DROP TRIGGER space_agent_state_set;
    DROP TRIGGER space_agent_state_changed;
    DROP TRIGGER space_agent_state_cleared;
    DROP INDEX space_agent_state_execution;
    ALTER TABLE space_agent_state RENAME TO workspace_agent_state;
    ALTER TABLE workspace_agent_state RENAME COLUMN space_id TO workspace_id;
    DROP TRIGGER workspace_numbered;
    DROP TRIGGER chat_started;
    CREATE TRIGGER chat_started AFTER INSERT ON chats BEGIN SELECT 1; END;
    DROP VIEW free_space_number;
    DROP VIEW space_numbers;
    DROP VIEW spaces;
    DROP INDEX questions_number;
    DROP INDEX access_request_number;
    ALTER TABLE questions DROP COLUMN space_number;
    ALTER TABLE access_requests DROP COLUMN space_number;
    CREATE UNIQUE INDEX questions_number ON questions(number);
    CREATE UNIQUE INDEX access_request_number ON access_requests(number);
    DROP INDEX workspaces_number;
    ALTER TABLE workspaces DROP COLUMN number;
    DROP TRIGGER chat_ended;
    CREATE TRIGGER chat_ended AFTER DELETE ON chats BEGIN SELECT 1; END;
    DROP INDEX chats_number;
    ALTER TABLE chats DROP COLUMN number;
    DROP INDEX chats_scope_token;
    ALTER TABLE chats DROP COLUMN scope_token;
    DROP TRIGGER workspace_created;
    CREATE TRIGGER workspace_created AFTER INSERT ON workspaces BEGIN SELECT 1; END;
    DROP TRIGGER space_parent_set;
    DROP TRIGGER space_parent_changed;
    DROP TRIGGER space_parent_cleared;
    DROP TRIGGER workspace_left_tree;
    DROP TRIGGER chat_left_tree;
    DROP TABLE space_parents;
    ALTER TABLE workspaces ADD COLUMN parent_workspace_id TEXT;
    CREATE INDEX workspaces_parent ON workspaces(parent_workspace_id);
    ALTER TABLE workspaces ADD COLUMN chat_id TEXT REFERENCES chats(id) ON DELETE SET NULL;
    CREATE INDEX workspaces_chat ON workspaces(chat_id);
    CREATE TRIGGER workspace_chat_changed AFTER UPDATE OF chat_id ON workspaces BEGIN SELECT 1; END;
    DROP TABLE changed_on_both_sides;
    DROP TRIGGER workspace_pending_updates_removed;
    DROP TRIGGER chat_pending_updates_removed;
    DROP TABLE pending_updates;
    DROP TRIGGER workspace_review_removed;
    DROP TRIGGER chat_review_removed;
    ALTER TABLE space_review RENAME TO workspace_review;
    ALTER TABLE workspace_review RENAME COLUMN space_id TO workspace_id;
    DROP TRIGGER workspace_links_removed;
    DROP TRIGGER chat_links_removed;
    ALTER TABLE space_issue RENAME TO workspace_issue;
    ALTER TABLE workspace_issue RENAME COLUMN space_id TO workspace_id;
    ALTER TABLE pr_cleanup RENAME COLUMN space_id TO workspace_id;
    ALTER TABLE space_items RENAME TO workspace_items;
    ALTER TABLE workspace_items RENAME COLUMN space_id TO workspace_id;
    ALTER TABLE pr_activity RENAME COLUMN space_id TO workspace_id;
    ALTER TABLE space_item_activity RENAME TO workspace_item_activity;
    ALTER TABLE workspace_item_activity RENAME COLUMN space_id TO workspace_id;
    DROP TABLE workspace_files;
    DROP TABLE ended_chats;
    ALTER TABLE chat_messages DROP COLUMN attachments;
    ALTER TABLE workspace_holds DROP COLUMN placed_by;
    ALTER TABLE executions DROP COLUMN agent_execution;
    ALTER TABLE executions DROP COLUMN asks_in_chat;
    ALTER TABLE questions DROP COLUMN in_chat;
    ALTER TABLE questions DROP COLUMN files;
    ALTER TABLE notifications DROP COLUMN files;
    ALTER TABLE executions DROP COLUMN model;
    ALTER TABLE executions DROP COLUMN effort;
    DROP TRIGGER workspace_grace_removed;
    DROP TRIGGER chat_grace_removed;
    DROP INDEX chats_workspace;
    ALTER TABLE chats DROP COLUMN workspace_id;
    ALTER TABLE chats DROP COLUMN stopped_at;
    ALTER TABLE chats DROP COLUMN saved;
    ALTER TABLE chats DROP COLUMN completion;
    ALTER TABLE chats DROP COLUMN session;
    ALTER TABLE chats DROP COLUMN asks_in_chat;
    DROP TRIGGER chat_saved;
    DROP TRIGGER chat_restored;
    ALTER TABLE chats DROP COLUMN restore;
    ALTER TABLE chats DROP COLUMN unique_id;
    ALTER TABLE chats DROP COLUMN stopped_by;
    ALTER TABLE workspaces DROP COLUMN start_commit;
    ALTER TABLE workspaces DROP COLUMN made_for;
    ALTER TABLE workspaces DROP COLUMN setup_skipped;
    ALTER TABLE workspaces DROP COLUMN disposable;
    ALTER TABLE workspaces DROP COLUMN narrowing;
    ALTER TABLE executions DROP COLUMN narrowing;
    ALTER TABLE chats DROP COLUMN narrowing;
    ALTER TABLE workspaces DROP COLUMN mode;
    ALTER TABLE executions DROP COLUMN mode;
    ALTER TABLE chats DROP COLUMN mode;
    PRAGMA user_version=87;";

/// Start a chat named `name` in the repository of `workspace` whose process
/// is `process`.
async fn chat(
    manager: &Manager,
    workspace: &Workspace,
    name: &str,
    process: &crate::daemon::test_support::ProcessGroup,
) -> crate::model::Chat {
    manager
        .start_chat(
            workspace.repository_id.clone().into(),
            name.into(),
            "claude".into(),
            process.id(),
            None,
            false,
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn a_chats_files_outlive_it_for_the_retention_period() {
    use crate::{
        daemon::scope::{self, Caller},
        protocol::FileMethod,
    };
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager).await;
    let process = crate::daemon::test_support::ProcessGroup::spawn(
        std::process::Command::new("sleep").arg("600"),
    );
    let chat = chat(&manager, &workspace, "planning", &process).await;
    let report = temp.path().join("report.md");
    std::fs::write(&report, "Plan").unwrap();
    let added = manager
        .add_attached_files(
            &SpaceSelector::from(&chat.name),
            vec![report.into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    let copy = added[0].path.clone();
    let attachments = workspace.path.parent().unwrap().join(".attachments");
    assert!(copy.starts_with(attachments.join(&chat.id)));
    let files = manager
        .attached_files(&SpaceSelector::id(&chat.id))
        .await
        .unwrap();
    assert_eq!(files, added);
    assert_eq!(
        (files[0].chat.as_deref(), files[0].expires_at),
        (Some("planning"), None)
    );
    // Deleting the files of every idle workspace spares a running chat's.
    assert!(manager.remove_idle_files().await.unwrap().is_empty());

    // A workspace's processes reach no chat's files.
    manager
        .executions
        .issue_scope(
            "worker".into(),
            Caller {
                space_id: workspace.id.clone(),
                execution: None,
            },
        )
        .await;
    let mut list = crate::protocol::Operation::from(FileMethod::ListFiles {
        space: SpaceSelector::parse(&chat.name),
    });
    assert!(
        scope::authorize(&manager, Some(&"worker".into()), &mut list)
            .await
            .is_err()
    );

    process.kill();
    manager.stop_finished_chats().await.unwrap();
    manager
        .remove_chat(&SpaceSelector::id(&chat.id), None)
        .await
        .unwrap();
    manager.expire_results().await.unwrap();
    let files = manager.attached_files(&"planning".into()).await.unwrap();
    let [kept] = files.as_slice() else {
        panic!("{files:?}");
    };
    assert_eq!(kept.source, FileSource::Added);
    assert!(kept.expires_at.is_some(), "{kept:?}");
    assert!(copy.exists());
    // A removed chat takes no more files.
    let late = temp.path().join("late.md");
    std::fs::write(&late, "Late").unwrap();
    assert!(
        manager
            .add_attached_files(
                &SpaceSelector::id(&chat.id),
                vec![late.into()],
                &Attacher::User
            )
            .await
            .is_err()
    );

    manager
        .store
        .run(|db| {
            db.execute("UPDATE ended_chats SET expires_at=0", [])?;
            Ok(())
        })
        .await
        .unwrap();
    // Files expiry cannot delete stay listed as the chat's.
    let directory = attachments.join(&chat.id);
    if permissions_bind() {
        use std::os::unix::fs::PermissionsExt;
        let set = |mode| {
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(mode)).unwrap()
        };
        set(0o500);
        let failed = manager.expire_results().await;
        set(0o700);
        assert!(failed.is_err());
        let files = manager.attached_files(&"planning".into()).await.unwrap();
        assert_eq!(files[0].chat.as_deref(), Some("planning"), "{files:?}");
        assert!(files[0].expires_at.is_some(), "{files:?}");
    }
    manager.expire_results().await.unwrap();
    assert!(!copy.exists());
    assert!(manager.attached_files(&"planning".into()).await.is_err());
    let left: i64 = manager
        .store
        .run(|db| Ok(db.query_row("SELECT COUNT(*) FROM ended_chats", [], |row| row.get(0))?))
        .await
        .unwrap();
    assert_eq!(left, 0);
}

#[tokio::test]
async fn a_chat_without_a_repository_keeps_its_files_in_root_dir() {
    let (temp, manager) = manager().await;
    let process =
        crate::test_support::ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    let new = crate::daemon::chats::NewChat {
        narrowing: Default::default(),
        name: "master".into(),
        agent: "claude".into(),
        mode: "orchestrate".into(),
        pid: process.id(),
        origin: None,
        hands_over: false,
        parent: None,
    };
    let chat = manager.start_new_chat(None, None, new).await.unwrap();
    let report = temp.path().join("report.md");
    std::fs::write(&report, "Plan").unwrap();
    let added = manager
        .add_attached_files(
            &SpaceSelector::from(&chat.name),
            vec![report.into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    let copy = added[0].path.clone();
    let root_dir = manager.config().root_dir(&manager.paths).unwrap();
    assert!(
        copy.starts_with(root_dir.join(".attachments")),
        "{}",
        copy.display()
    );
    assert_eq!(
        manager
            .attached_files(&SpaceSelector::id(&chat.id))
            .await
            .unwrap(),
        added
    );
    // Deleting the files of every idle space spares a running chat's.
    assert!(manager.remove_idle_files().await.unwrap().is_empty());

    process.kill();
    manager.stop_finished_chats().await.unwrap();
    manager
        .remove_chat(&SpaceSelector::id(&chat.id), None)
        .await
        .unwrap();
    manager.expire_results().await.unwrap();
    assert!(copy.exists());
    manager
        .store
        .run(|db| {
            db.execute("UPDATE ended_chats SET expires_at=0", [])?;
            Ok(())
        })
        .await
        .unwrap();
    manager.expire_results().await.unwrap();
    assert!(!copy.exists());
    assert!(manager.attached_files(&"master".into()).await.is_err());
}

#[tokio::test]
async fn files_of_a_chat_without_a_repository_stay_with_the_root_dir_it_started_in() {
    let (temp, manager) = manager().await;
    let process =
        crate::test_support::ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    let new = crate::daemon::chats::NewChat {
        narrowing: Default::default(),
        name: "master".into(),
        agent: "claude".into(),
        mode: "orchestrate".into(),
        pid: process.id(),
        origin: None,
        hands_over: false,
        parent: None,
    };
    let chat = manager.start_new_chat(None, None, new).await.unwrap();
    let root_dir = manager.config().root_dir(&manager.paths).unwrap();
    let space = SpaceSelector::from(&chat.name);
    let attach = |name: &str| {
        let file = temp.path().join(name);
        std::fs::write(&file, name).unwrap();
        manager.add_attached_files(&space, vec![file.into()], &Attacher::User)
    };
    let early = attach("early.md").await.unwrap()[0].path.clone();
    // root_dir changes while the chat runs, as an edit and a reload change it.
    let moved = std::fs::canonicalize(temp.path()).unwrap().join("moved");
    let config = format!("[machine]\nroot_dir = {moved:?}\n");
    crate::daemon::test_support::configure(&manager, &config).await;
    let late = attach("late.md").await.unwrap()[0].path.clone();
    for copy in [&early, &late] {
        assert!(
            copy.starts_with(root_dir.join(".attachments")),
            "{}",
            copy.display()
        );
    }
    // Deleting one still finds it where it is kept.
    manager
        .remove_attached_files(
            &SpaceSelector::id(&chat.id),
            vec!["early.md".into()],
            Vec::new(),
        )
        .await
        .unwrap();
    assert!(!early.exists());

    process.kill();
    manager.stop_finished_chats().await.unwrap();
    manager
        .remove_chat(&SpaceSelector::id(&chat.id), None)
        .await
        .unwrap();
    manager
        .store
        .run(|db| {
            db.execute("UPDATE ended_chats SET expires_at=0", [])?;
            Ok(())
        })
        .await
        .unwrap();
    manager.expire_results().await.unwrap();
    assert!(!late.exists(), "{}", late.display());
    assert!(manager.attached_files(&"master".into()).await.is_err());
}

#[tokio::test]
async fn a_chats_agent_attaches_as_its_chat_outside_any_workspace() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager).await;
    let process = crate::daemon::test_support::ProcessGroup::spawn(
        std::process::Command::new("sleep").arg("600"),
    );
    let chat = chat(&manager, &workspace, "planning", &process).await;
    let as_chat = Attacher::Chat {
        agent: "claude".into(),
        chat: chat.id.clone(),
    };
    assert_eq!(manager.attacher(None, Some(&chat.id)).await, as_chat);
    // Workspace scope comes first, as for a command a chat's agent runs
    // in a workspace.
    let terminal = Caller {
        space_id: workspace.id.clone(),
        execution: None,
    };
    assert_eq!(
        manager.attacher(Some(&terminal), Some(&chat.id)).await,
        Attacher::User
    );
    // A chat's agent attaching to a workspace attaches as its chat too.
    let report = temp.path().join("report.md");
    std::fs::write(&report, "Plan").unwrap();
    let attacher = manager.attacher(None, Some(&chat.id)).await;
    let added = manager
        .add_attached_files(
            &SpaceSelector::from(&workspace.name),
            vec![report.into()],
            &attacher,
        )
        .await
        .unwrap();
    assert_eq!(added[0].attached_by, Some(as_chat));

    process.kill();
    assert_eq!(manager.attacher(None, Some(&chat.id)).await, Attacher::User);
}

mod events;
