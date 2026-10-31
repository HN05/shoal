use std::time::Duration;

use super::{Attacher, FileSource};
use crate::{
    daemon::{
        scope::Caller,
        workspace::{ExecutionKind, Manager},
    },
    execution::recovery,
    model::Workspace,
    test_support::{manager, repository},
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
        .begin_execution(&workspace.id, None, ExecutionKind::Command, caller)
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
            name,
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
        workspace_id: workspace.id.clone(),
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
        .add_attached_files(&workspace.name, vec![file.into()], &attacher)
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
        .add_attached_files(&workspace.name, vec![notes.into()], &attacher)
        .await
        .unwrap();
    manager
        .send_agent_message(
            &workspace.name,
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
    let recorded: Option<String> = manager
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
    let files = manager.attached_files(&workspace.name).await.unwrap();
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
        .mark_done(&workspace.id, false, false, vec![report.into()], &attacher)
        .await
        .unwrap();
    let files = manager.attached_files(&workspace.name).await.unwrap();
    let [result] = files.as_slice() else {
        panic!("{files:?}");
    };
    assert_eq!(result.source, FileSource::Result);
    assert_eq!(
        result.attached_by,
        Some(Attacher::Agent {
            agent: "codex".into(),
            execution: caller.execution_id().unwrap().into(),
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
            .attached_files(&workspace.name)
            .await
            .unwrap()
            .is_empty()
    );
    // Sending the same file again keeps one record of its copy.
    for _ in 0..2 {
        manager
            .send_agent_message(
                &workspace.name,
                "See the log".into(),
                vec![file.clone().into()],
                &Attacher::User,
            )
            .await
            .unwrap();
    }
    let files = manager.attached_files(&workspace.name).await.unwrap();
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
            &workspace.id,
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
        .add_attached_files(&workspace.name, vec![file.clone().into()], &Attacher::User)
        .await
        .unwrap();
    let [copy] = added.as_slice() else {
        panic!("{added:?}");
    };
    assert_eq!(copy.source, FileSource::Added);
    assert_eq!(std::fs::read_to_string(&copy.path).unwrap(), "port = 1");
    // Adding it again keeps the copy; sending it is a separate copy.
    manager
        .add_attached_files(&workspace.name, vec![file.clone().into()], &Attacher::User)
        .await
        .unwrap();
    manager
        .send_agent_message(
            &workspace.name,
            String::new(),
            vec![file.into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    let files = manager.attached_files(&workspace.name).await.unwrap();
    let sources: Vec<_> = files.iter().map(|file| file.source).collect();
    assert_eq!(sources, [FileSource::Added, FileSource::Message]);
    assert!(
        manager
            .agent_messages(&workspace.name)
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
            &workspace.name,
            vec![titled(&chart, "Latency by region")],
            &Attacher::User,
        )
        .await
        .unwrap();
    // Adding it again without a title keeps the title.
    manager
        .add_attached_files(&workspace.name, vec![chart.clone().into()], &Attacher::User)
        .await
        .unwrap();
    manager
        .send_agent_message(
            &workspace.name,
            String::new(),
            vec![titled(&shot, "Login screen"), report.clone().into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    manager
        .mark_done(
            &workspace.id,
            false,
            false,
            vec![titled(&report, "Audit findings")],
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
    let listed = manager.attached_files(&workspace.name).await.unwrap();
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
            &workspace.name,
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
    let error = manager
        .add_attached_files(
            &workspace.name,
            vec![super::NewFile {
                path: file,
                title: Some("Latency\nby region".into()),
            }],
            &Attacher::User,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("file titles"), "{error:#}");
    assert!(
        manager
            .attached_files(&workspace.name)
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
            &workspace.name,
            "See".into(),
            vec![log.clone().into(), shot.into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    let error = manager
        .remove_attached_files(&workspace.name, vec!["missing.txt".into()], vec![])
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("no attached file missing.txt"),
        "{error:#}"
    );
    let deleted = manager
        .remove_attached_files(&workspace.name, vec!["crash.log".into()], vec![])
        .await
        .unwrap();
    assert!(!deleted[0].path.exists());
    let left = manager.attached_files(&workspace.name).await.unwrap();
    let [kept] = left.as_slice() else {
        panic!("{left:?}");
    };
    assert_eq!(kept.name, "shot.png");
    assert!(kept.path.exists());

    // Sending the same files again copies the deleted one anew.
    manager
        .send_agent_message(
            &workspace.name,
            "See".into(),
            vec![log.into(), temp.path().join("shot.png").into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    assert!(deleted[0].path.exists());
    assert_eq!(
        manager.attached_files(&workspace.name).await.unwrap().len(),
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
            .add_attached_files(&workspace.name, vec![file.clone().into()], &Attacher::User)
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
        assert_eq!(manager.attached_files(&spared.name).await.unwrap().len(), 1);
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
        .split_workspace(&parent.id, "audit".into(), None)
        .await
        .unwrap();
    let report = temp.path().join("report.md");
    std::fs::write(&report, "No leaks").unwrap();
    let completion = manager
        .mark_done(
            &child.id,
            false,
            false,
            vec![report.into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    assert_eq!(completion.results[0].name, "report.md");
    let result = manager.attached_files(&child.id).await.unwrap()[0]
        .path
        .clone();
    assert_eq!(std::fs::read_to_string(&result).unwrap(), "No leaks");
    assert!(
        manager
            .notifications(true, 50)
            .await
            .unwrap()
            .iter()
            .any(|n| n.kind == NotificationKind::WorkspaceDone
                && n.message
                    .ends_with("; results in shoal files audit: report.md")),
    );

    manager
        .remove_workspace(
            &child.id,
            crate::removal::UnsavedWork::default(),
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    manager.expire_results().await.unwrap();
    let files = manager.attached_files("audit").await.unwrap();
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
                    workspace_id: workspace.id.clone(),
                    execution: None,
                },
            )
            .await;
        let mut list = crate::protocol::Operation::from(FileMethod::ListFiles {
            space: "audit".into(),
        });
        let authorized = scope::authorize(&manager, Some(token), &mut list).await;
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
    assert!(manager.attached_files("audit").await.is_err());
}

#[tokio::test]
async fn results_stay_reachable_when_a_workspace_between_goes() {
    use crate::daemon::scope::{self, Caller};
    use crate::protocol::FileMethod;
    let (temp, manager) = manager().await;
    let top = workspace(&temp, &manager).await;
    let middle = manager
        .split_workspace(&top.id, "middle".into(), None)
        .await
        .unwrap();
    let bottom = manager
        .split_workspace(&middle.id, "bottom".into(), None)
        .await
        .unwrap();
    let report = temp.path().join("report.md");
    std::fs::write(&report, "No leaks").unwrap();
    manager
        .mark_done(
            &bottom.id,
            false,
            false,
            vec![report.into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    for removed in [&bottom, &middle] {
        manager
            .remove_workspace(
                &removed.id,
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
                workspace_id: top.id.clone(),
                execution: None,
            },
        )
        .await;
    let mut list = crate::protocol::Operation::from(FileMethod::ListFiles {
        space: "bottom".into(),
    });
    scope::authorize(&manager, Some("top"), &mut list)
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
        .add_attached_files(&workspace.name, vec![file.into()], &Attacher::User)
        .await
        .unwrap();
    // The copy's directory now leads elsewhere, to a file of the same name.
    let elsewhere = temp.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("notes.md"), "keep me").unwrap();
    let directory = added[0].path.parent().unwrap();
    std::fs::remove_dir_all(directory).unwrap();
    std::os::unix::fs::symlink(&elsewhere, directory).unwrap();
    let error = manager
        .remove_attached_files(&workspace.name, vec![], vec![])
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("is not among the repository's attached files"),
        "{error:#}"
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
            &audit.id,
            false,
            false,
            vec![report.into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    let result = manager.attached_files(&audit.id).await.unwrap()[0]
        .path
        .clone();
    manager
        .remove_workspace(
            &audit.id,
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
    let error = manager.expire_results().await.unwrap_err();
    assert!(
        error
            .to_string()
            .contains("is not among the repository's attached files"),
        "{error:#}"
    );
    assert!(elsewhere.join(&audit.id).join("keep.md").exists());
}

#[tokio::test]
async fn a_completion_hands_back_a_bounded_number_of_files() {
    let (temp, manager) = manager().await;
    let audit = workspace(&temp, &manager).await;
    let results = (0..=super::MAX_RESULTS)
        .map(|n| temp.path().join(format!("{n}.md")).into())
        .collect();
    let error = manager
        .mark_done(&audit.id, false, false, results, &Attacher::User)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("at most"), "{error:#}");
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
            &audit.id,
            false,
            false,
            vec![report.into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    let renamed = manager
        .rename_workspace(&audit.id, "renamed", None)
        .await
        .unwrap();
    manager
        .remove_workspace(
            &audit.id,
            crate::removal::UnsavedWork::default(),
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    let files = manager.attached_files(&renamed.name).await.unwrap();
    assert_eq!(files[0].workspace, Some(renamed.name.clone()));
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
        .add_attached_files(&workspace.name, files.clone(), &Attacher::User)
        .await
        .unwrap();
    // The workspace's directory now leads elsewhere, where a copy is gone.
    let directory = added[0].path.ancestors().nth(3).unwrap().to_owned();
    let elsewhere = temp.path().join("elsewhere");
    std::fs::rename(&directory, &elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &directory).unwrap();
    let inside = added[1].path.strip_prefix(&directory).unwrap();
    std::fs::remove_file(elsewhere.join(added[0].path.strip_prefix(&directory).unwrap())).unwrap();
    let error = manager
        .add_attached_files(&workspace.name, files, &Attacher::User)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("is not among the repository's attached files"),
        "{error:#}"
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
        .add_attached_files(&workspace.name, vec![file.clone().into()], &Attacher::User)
        .await
        .unwrap();
    manager
        .send_agent_message(
            &workspace.name,
            String::new(),
            vec![file.into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    let deleted = manager
        .remove_attached_files(&workspace.name, vec![], vec![added[0].id])
        .await
        .unwrap();
    assert_eq!(deleted, added);
    let left = manager.attached_files(&workspace.name).await.unwrap();
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
            &audit.id,
            false,
            false,
            vec![report.clone().into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    // The same file again changes nothing.
    let error = manager
        .mark_done(
            &audit.id,
            false,
            false,
            vec![report.clone().into()],
            &Attacher::User,
        )
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("already marked done"),
        "{error:#}"
    );
    std::fs::write(&report, "Revised").unwrap();
    let revised = manager
        .mark_done(
            &audit.id,
            false,
            false,
            vec![report.into()],
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
            &workspace.name,
            "See".into(),
            vec![log.into(), env.into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    let copies: Vec<_> = manager
        .attached_files(&workspace.name)
        .await
        .unwrap()
        .into_iter()
        .map(|file| file.path)
        .collect();
    // A state database from before Shoal recorded attached files.
    manager
        .store
        .run(|db| {
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
            .attached_files(&workspace.name)
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
    let files = reopened.attached_files(&workspace.name).await.unwrap();
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
        .add_attached_files(&workspace.name, vec![file.into()], &Attacher::User)
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
        manager.attached_files(&workspace.name).await.unwrap().len(),
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
            &workspace.name,
            "See".into(),
            vec![file.into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    let copy = manager.attached_files(&workspace.name).await.unwrap()[0]
        .path
        .clone();
    manager
        .store
        .run(|db| {
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
    let files = reopened.attached_files(&workspace.name).await.unwrap();
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
            &workspace.name,
            "See".into(),
            vec![file.into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    manager
        .store
        .run(|db| {
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
        .join(crate::fsutil::OWNER_MARKER);
    std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o000)).unwrap();
    let failed = reopened.record_earlier_files().await;
    std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(failed.is_err(), "an unreadable marker fails the pass");
    reopened.record_earlier_files().await.unwrap();
    assert_eq!(
        reopened
            .attached_files(&workspace.name)
            .await
            .unwrap()
            .len(),
        1
    );
    reopened.store.shutdown().await;
}

/// Return a current state database to version 87, before Shoal recorded
/// attached files, undoing the later migrations that cannot run twice.
const BEFORE_ATTACHED_FILES: &str = "DROP TABLE workspace_files;
    DROP TABLE ended_chats;
    ALTER TABLE workspace_holds DROP COLUMN placed_by;
    ALTER TABLE executions DROP COLUMN agent_execution;
    ALTER TABLE executions DROP COLUMN asks_in_chat;
    ALTER TABLE questions DROP COLUMN in_chat;
    PRAGMA user_version=87;";

/// Start a chat named `name` in the repository of `workspace` whose process
/// is `process`.
async fn chat(
    manager: &Manager,
    workspace: &Workspace,
    name: &str,
    process: &std::process::Child,
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
    let mut process = std::process::Command::new("sleep")
        .arg("600")
        .spawn()
        .unwrap();
    let chat = chat(&manager, &workspace, "planning", &process).await;
    let report = temp.path().join("report.md");
    std::fs::write(&report, "Plan").unwrap();
    let added = manager
        .add_attached_files(&chat.name, vec![report.into()], &Attacher::User)
        .await
        .unwrap();
    let copy = added[0].path.clone();
    assert!(
        copy.starts_with(
            workspace
                .path
                .parent()
                .unwrap()
                .join(".attachments")
                .join(&chat.id)
        )
    );
    let files = manager.attached_files(&chat.id).await.unwrap();
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
                workspace_id: workspace.id.clone(),
                execution: None,
            },
        )
        .await;
    let mut list = crate::protocol::Operation::from(FileMethod::ListFiles {
        space: chat.name.clone(),
    });
    assert!(
        scope::authorize(&manager, Some("worker"), &mut list)
            .await
            .is_err()
    );

    process.kill().unwrap();
    process.wait().unwrap();
    manager.end_finished_chats().await.unwrap();
    manager.expire_results().await.unwrap();
    let files = manager.attached_files("planning").await.unwrap();
    let [kept] = files.as_slice() else {
        panic!("{files:?}");
    };
    assert_eq!(kept.source, FileSource::Added);
    assert!(kept.expires_at.is_some(), "{kept:?}");
    assert!(copy.exists());
    // An ended chat takes no more files.
    let late = temp.path().join("late.md");
    std::fs::write(&late, "Late").unwrap();
    assert!(
        manager
            .add_attached_files(&chat.id, vec![late.into()], &Attacher::User)
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
    manager.expire_results().await.unwrap();
    assert!(!copy.exists());
    assert!(manager.attached_files("planning").await.is_err());
}

#[tokio::test]
async fn a_chats_agent_attaches_as_its_chat_outside_any_workspace() {
    let (temp, manager) = manager().await;
    let workspace = workspace(&temp, &manager).await;
    let mut process = std::process::Command::new("sleep")
        .arg("600")
        .spawn()
        .unwrap();
    let chat = chat(&manager, &workspace, "planning", &process).await;
    let as_chat = Attacher::Chat {
        agent: "claude".into(),
        chat: chat.id.clone(),
    };
    assert_eq!(manager.attacher(None, Some(&chat.name)).await, as_chat);
    // Workspace scope comes first, as for a command a chat's agent runs
    // in a workspace.
    let terminal = Caller {
        workspace_id: workspace.id.clone(),
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
        .add_attached_files(&workspace.name, vec![report.into()], &attacher)
        .await
        .unwrap();
    assert_eq!(added[0].attached_by, Some(as_chat));

    process.kill().unwrap();
    process.wait().unwrap();
    assert_eq!(manager.attacher(None, Some(&chat.id)).await, Attacher::User);
}
