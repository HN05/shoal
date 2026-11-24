use super::*;
use crate::protocol::{AlertMethod, ThreadMethod};
use crate::threads::{ThreadKind, ThreadOpening};

#[test]
fn a_withdrawal_targets_a_space_only_when_it_names_the_recipient() {
    let withdraw = |space: Option<SpaceSelector>| {
        Operation::Alert(AlertMethod::WithdrawAlert {
            id: 7,
            space,
            from: None,
        })
    };
    assert_eq!(withdraw(None).target(), None);
    let space = SpaceSelector::Number(3.into());
    let named = withdraw(Some(space.clone()));
    assert_eq!(named.target(), Some(Target::Space(&space)));
}

#[test]
fn a_thread_opened_as_a_space_targets_none_and_one_opened_with_a_space_acts_there() {
    let open = |from: Option<SpaceSelector>, to: Option<SpaceSelector>| {
        Operation::Thread(ThreadMethod::OpenThread {
            opening: ThreadOpening::named("Which base?"),
            kind: Some(ThreadKind::Question),
            to,
            from,
        })
    };
    let to = SpaceSelector::Number(4.into());
    let from = SpaceSelector::Number(2.into());
    let with = open(None, Some(to.clone()));
    assert_eq!(with.target(), Some(Target::Acting(&to)));
    assert_eq!(open(Some(from), Some(to)).target(), None);
}

#[test]
fn a_thread_named_by_its_own_number_alone_targets_none() {
    let show = |thread| Operation::Thread(ThreadMethod::ShowThread { thread });
    let full = ItemNumber {
        space: 5.into(),
        item: 2,
    };
    let shown = show(ThreadNumber::Full(full));
    assert_eq!(shown.target(), Some(Target::Item(&full)));
    assert_eq!(show(ThreadNumber::Own(2)).target(), None);
}

#[test]
fn a_forge_request_targets_the_space_of_its_context_and_none_of_a_checkout() {
    let space = SpaceSelector::Number(6.into());
    let labels = |context| {
        Operation::Item(crate::protocol::ItemMethod::Labels {
            target: RepoTarget {
                context: Some(context),
                repository: None,
            },
        })
    };
    let in_space = labels(ItemContext::space(space.clone()));
    assert_eq!(in_space.target(), Some(Target::Space(&space)));
    let checkout = labels(ItemContext::Checkout(
        shoal_ids::repository_ids::RepositoryId::generate(),
    ));
    assert_eq!(checkout.target(), None);
}

#[test]
fn a_request_with_files_attached_here_is_one_that_attaches_files() {
    let method = |attachments: serde_json::Value| serde_json::json!({"send_alert": {"to": {"space": {"space": "7"}}, "message": "hi", "attachments": attachments}});
    let attached = method(serde_json::json!([{"path": "/tmp/report.md"}]));
    assert!(attaches_files(&attached));
    assert!(!attaches_files(&method(serde_json::json!([]))));
    let rename = serde_json::json!({"rename_space": {"space": "7", "name": "x"}});
    assert!(!attaches_files(&rename));
    // An issue's files sit within its content.
    let issue = serde_json::json!({"open_issue": {"space": "7", "issue": {"title": "t",
        "attachments": [{"path": "/tmp/chart.png"}]}, "link": false}});
    assert!(attaches_files(&issue));
}

#[test]
fn an_unmerge_and_a_preparation_start_on_their_workspace_s_machine_without_the_caller_s_env() {
    use crate::protocol::operations::{OperationSpec, Preparation};
    let space = SpaceSelector::Number(8.into());
    let start = |spec| {
        Operation::Operations(OperationMethod::StartOperation {
            spec,
            env: Some([("HOME".to_owned(), "/caller".to_owned())].into()),
        })
    };
    let unmerge = OperationSpec::Unmerge {
        workspace: space.clone(),
    };
    let prepare = OperationSpec::Prepare {
        workspace: space.clone(),
        steps: Preparation::default(),
    };
    for spec in [unmerge, prepare] {
        let mut started = start(spec);
        assert_eq!(started.target(), Some(Target::Space(&space)));
        assert_eq!(started.forwarding(), Forwarding::Online);
        started.leave_caller_machine();
        let Operation::Operations(OperationMethod::StartOperation { env, .. }) = started else {
            unreachable!()
        };
        assert_eq!(env, None);
    }
    let open_pull = start(OperationSpec::OpenPull {
        workspace: space,
        options: Default::default(),
    });
    assert_eq!(open_pull.target(), None);
    assert_eq!(open_pull.forwarding(), Forwarding::Never);
}

#[test]
fn a_move_and_the_read_of_its_package_reach_the_machine_that_keeps_a_removed_record() {
    let id = shoal_ids::space_ids::SpaceId::from("gone".to_owned());
    let space = SpaceSelector::id(&id);
    let order = crate::transfer::MoveOrder {
        spaces: vec![id],
        destination: crate::transfer::MoveDestination::Held,
        recursive: true,
        keep: false,
        checkpoint: None,
        consent: None,
        check_only: false,
    };
    let moving = Operation::Operations(OperationMethod::StartMove {
        order: Box::new(order),
        env: None,
    });
    assert_eq!(moving.kept(), Some(Keeps::Record));
    let reading = Operation::Removal(RemovalMethod::ReadHeldPackage {
        space: space.clone(),
        copy: true,
    });
    assert_eq!(reading.kept(), Some(Keeps::Record));
    let stopping = Operation::Removal(RemovalMethod::CheckStop { space });
    assert_eq!(stopping.kept(), None);
}
