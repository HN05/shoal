use super::*;
use crate::{
    connection::Connector,
    paths::Paths,
    protocol::{ChatScope, Operation, Request, WorkspaceMethod},
};
use serde_json::json;

const SECRET: &str = "5f0c3e1a-scope-secret";

#[test]
fn structs_holding_a_token_never_debug_it() {
    let reattach: Request = serde_json::from_value(json!({
        "protocol": 1, "id": 1, "scope": SECRET,
        "method": {"reattach": {
            "execution": "e1", "wrapper": {"pid": 2, "birth": "b"},
            "child": null, "group_id": 2, "scope_token": SECRET
        }}
    }))
    .unwrap();
    let revoke = Operation::Workspace(WorkspaceMethod::RevokeWorkspaceEnv {
        workspace: crate::space_names::SpaceSelector::id(&"w1".into()),
        token: SECRET.parse().unwrap(),
    });
    let chat = ChatScope {
        token: ScopeToken::from(SECRET.to_owned()),
    };
    let connector = Connector {
        paths: Paths::new(Some("/state".into())).unwrap(),
        scope: Some(ScopeToken::from(SECRET.to_owned())),
    };
    for debugged in [
        format!("{reattach:?}"),
        format!("{revoke:?}"),
        format!("{chat:?}"),
        format!("{connector:?}"),
    ] {
        assert!(!debugged.contains(SECRET), "{debugged}");
        assert!(debugged.contains("ScopeToken"), "{debugged}");
    }
    let wire = serde_json::to_value(&reattach).unwrap();
    assert_eq!(wire["scope"], SECRET);
    assert_eq!(wire["method"]["reattach"]["scope_token"], SECRET);
}
