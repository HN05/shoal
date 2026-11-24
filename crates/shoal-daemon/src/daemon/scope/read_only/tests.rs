use crate::daemon::{
    scope::{Access, Caller, authorize},
    test_support::{manager, repository, workspace_below},
};
use crate::protocol::{
    AgentMethod, MergingMethod, Operation, ReviewMethod, SpaceMethod, ThreadMethod,
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::reviews::ReviewId;
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::threads::{NewPost, ThreadKind, ThreadNumber, ThreadOpening};

fn caller(space: &SpaceId, access: Access) -> Caller {
    Caller {
        space_id: space.clone(),
        execution: None,
        granted: None,
        access,
        vouched: None,
    }
}

/// A read-only scope reads the spaces below its own, and is refused what
/// the same scope may otherwise do there: a read that changes the agent's
/// watch, and a change. A review run's and an answer run's read as it does.
#[tokio::test]
async fn read_only_scope_reads_without_changing() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repository = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repository.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let child = workspace_below(&manager, &repository, "child", &parent.id, None).await;
    let operations = |space: &SpaceId| {
        let space = SpaceSelector::id(space);
        [
            Operation::Space(SpaceMethod::SpaceStatus {
                space: space.clone(),
            }),
            // Listing a space's alerts leaves them unread.
            Operation::Alert(crate::protocol::AlertMethod::ListAlerts {
                to: crate::protocol::AlertsFor::Space {
                    space: space.clone(),
                },
                unread_only: true,
                limit: 50,
            }),
            Operation::Agent(AgentMethod::AgentUpdates {
                space: space.clone(),
            }),
            Operation::Alert(crate::protocol::AlertMethod::MarkAlertsRead {
                ids: Vec::new(),
                elsewhere: Default::default(),
            }),
            // Its review gate may queue a review.
            Operation::Merging(MergingMethod::PreviewMerge {
                workspace: space,
                push: Some(false),
                method: None,
                message: None,
            }),
        ]
    };
    let accesses = [
        Access::Full,
        Access::ReadOnly,
        Access::Review(ReviewId(1)),
        Access::Answer(1),
    ];
    for access in accesses {
        let token = manager.operations.issue(&caller(&parent.id, access));
        let mut allowed = Vec::new();
        for mut operation in operations(&child.id) {
            let authorized = authorize(&manager, Some(&token), &mut operation).await;
            allowed.push(authorized.is_ok());
        }
        let expected = match access {
            Access::Full => [true, true, true, true, true],
            _ => [true, true, false, false, false],
        };
        assert_eq!(allowed, expected, "{access:?}");
    }
}

/// A review run's scope writes only its review: a thread without a kind,
/// a post and a submit, which the daemon hands that review; a question and
/// any other change are refused. An answer run's only posts.
#[tokio::test]
async fn a_review_or_answer_runs_scope_writes_only_its_own() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repository = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repository.id, "reviewed".into(), None, None, None)
        .await
        .unwrap();
    let open = |kind| {
        Operation::Thread(ThreadMethod::OpenThread {
            opening: ThreadOpening::named("Unclear"),
            kind,
            to: None,
            from: None,
        })
    };
    let operations = || {
        [
            open(None),
            Operation::Thread(ThreadMethod::Post {
                thread: ThreadNumber::Own(1),
                post: NewPost::default(),
                attachments: Vec::new(),
                code: Vec::new(),
                changes: Vec::new(),
                from: None,
            }),
            Operation::Review(ReviewMethod::SubmitReview {
                workspace: None,
                approve: false,
                summary: None,
            }),
            open(Some(ThreadKind::Question)),
            Operation::Review(ReviewMethod::CancelReview {
                workspace: None,
                id: None,
                here: false,
            }),
        ]
    };
    let mut allowed = Vec::new();
    for access in [Access::Review(ReviewId(1)), Access::Answer(1)] {
        let token = (manager.operations).issue(&caller(&workspace.id, access));
        for mut operation in operations() {
            let authorized = authorize(&manager, Some(&token), &mut operation).await;
            allowed.push(authorized.is_ok());
        }
    }
    let review = [true, true, true, false, false];
    // An answer run posts, which coordination keeps to its thread.
    let answer = [false, true, false, false, false];
    assert_eq!(allowed, [review, answer].concat());
}
