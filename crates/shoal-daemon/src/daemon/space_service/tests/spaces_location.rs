use shoal_protocol::protocol::{AlertMethod, AlertTarget, Operation};
use shoal_protocol::space_names::SpaceSelector;

use crate::daemon::test_daemon::TestDaemon;
use shoal_protocol::model::Located;

/// A space another machine, the laptop, lists below `parent`.
fn listed(id: &str, parent: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "machine": "laptop", "id": id, "number": 1, "name": id, "kind": "chat",
        "status": "working", "parent": parent, "reported_at": 0,
    })
}

#[tokio::test]
async fn a_walk_down_the_tree_crosses_between_machines_as_the_tree_does() {
    let daemon = TestDaemon::start().await;
    // This machine's space below the laptop's child, as the hub lists it.
    let here = daemon.workspace("grandchild").await;
    let view: shoal_protocol::hub::directory::HubView = serde_json::from_value(serde_json::json!({
        "machine": "desk",
        "machines": [],
        "spaces": [listed("top", None), listed("child", Some("top")), listed("aside", None)],
        "below_elsewhere": [[here.id, "child"]],
    }))
    .unwrap();
    let top = view.directory.spaces[0].clone();
    let tree = crate::daemon::spaces::location::Tree {
        here: vec![shoal_protocol::model::Space::from(here.clone())],
        view: Some(view),
    };
    let root = Located::Elsewhere {
        space: Box::new(top),
        machine: Box::new(shoal_protocol::hub::Member::unlisted("laptop".into())),
    };
    let found: Vec<String> = (tree.walk(vec![root]).iter())
        .map(|located| match located {
            Located::Here(space) => format!("here {}", space.id),
            Located::Elsewhere { space, .. } => format!("elsewhere {}", space.space.id),
        })
        .collect();
    let expected = [
        "elsewhere top".to_owned(),
        "elsewhere child".to_owned(),
        format!("here {}", here.id),
    ];
    assert_eq!(found, expected);
}

/// Whether `manager` offers a picker for `acting` the space `away` of
/// another machine.
async fn offers_away(manager: &crate::daemon::manager::Manager, acting: Operation) -> bool {
    let everywhere = manager
        .spaces
        .spaces_everywhere(manager, None, false, Some(&acting));
    let elsewhere = everywhere.await.unwrap().elsewhere;
    elsewhere.is_some_and(|view| {
        (view.directory.spaces.iter()).any(|listed| listed.space.id.as_str() == "away")
    })
}

#[tokio::test]
async fn a_picker_is_offered_the_other_machines_spaces_a_request_reaches() {
    let daemon = TestDaemon::start().await;
    let manager = &daemon.manager;
    crate::daemon::test_support::away_on_offline_machine(manager, None).await;
    let send = |attachments| {
        Operation::from(AlertMethod::SendAlert {
            to: AlertTarget::Space {
                space: SpaceSelector::Number(90.into()),
            },
            from: None,
            message: "Rebase onto main".into(),
            attachments,
        })
    };

    assert!(offers_away(manager, send(Vec::new())).await);
    // This machine's file, which the other machine cannot read.
    let file = shoal_protocol::files::NewFile::from(std::path::PathBuf::from("/tmp/report.md"));
    assert!(!offers_away(manager, send(vec![file])).await);
}
