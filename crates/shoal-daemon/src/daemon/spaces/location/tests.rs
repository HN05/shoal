use shoal_protocol::space_names::SpaceSelector;

use crate::daemon::{scope::Caller, test_daemon::TestDaemon};
use crate::refusal::Reason;
use shoal_protocol::model::Located;

#[tokio::test]
async fn a_space_here_is_located_here_and_one_outside_the_caller_s_scope_is_not() {
    let daemon = TestDaemon::start().await;
    let manager = &daemon.manager;
    let (own, other) = (
        daemon.workspace("own").await,
        daemon.workspace("other").await,
    );
    let selector = SpaceSelector::id(&other.id);
    let located = manager.locate_space(None, &selector).await.unwrap();
    assert!(matches!(&located, Located::Here(space) if space.id == other.id));
    // A sibling is outside the scope of the other space's process.
    let caller = Caller::of_space(own.id.clone());
    let refused = manager.locate_space(Some(&caller), &selector).await;
    assert_eq!(Reason::of(&refused.unwrap_err()), Some(Reason::NotFound));
}

#[tokio::test]
async fn a_space_no_machine_has_is_refused_as_not_found() {
    let daemon = TestDaemon::start().await;
    let unknown = SpaceSelector::Number(99.into());
    let refused = daemon.manager.locate_space(None, &unknown).await;
    assert_eq!(Reason::of(&refused.unwrap_err()), Some(Reason::NotFound));
}

#[tokio::test]
async fn a_stop_s_selection_takes_the_spaces_below_after_the_one_above_only_when_asked() {
    let daemon = TestDaemon::start().await;
    let manager = &daemon.manager;
    // A space beside it stays out.
    let (top, child, _aside) = (
        daemon.workspace("top").await,
        daemon.workspace("child").await,
        daemon.workspace("aside").await,
    );
    let (below, above) = (child.id.clone(), top.id.clone());
    (manager.store)
        .write(move |tx| crate::daemon::tree::set_parent(tx, &below, Some(&above)))
        .await
        .unwrap();
    let selected = [SpaceSelector::id(&top.id)];
    let ids = |located: Vec<Located>| -> Vec<_> {
        (located.iter())
            .map(|located| located.id().clone())
            .collect()
    };
    // Named twice, by ID and number, it is taken once.
    let twice = [
        SpaceSelector::id(&top.id),
        SpaceSelector::Number(top.number),
    ];
    let alone = manager
        .select_spaces(None, &twice, &[], false)
        .await
        .unwrap();
    assert_eq!(ids(alone), std::slice::from_ref(&top.id));
    let tree = manager
        .select_spaces(None, &selected, &[], true)
        .await
        .unwrap();
    assert_eq!(ids(tree), [top.id.clone(), child.id.clone()]);
}

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
    let tree = super::Tree {
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
