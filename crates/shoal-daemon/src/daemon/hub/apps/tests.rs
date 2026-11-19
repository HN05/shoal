use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use shoal_ids::member_ids::MemberId;
use shoal_protocol::{
    apps::store::{AppScope, Write},
    hub::{Member, MemberKind},
    protocol::{AppMethod, Body, ErrorCode, RemoteError, Response},
};

use crate::{
    daemon::{manager::Manager, test_support},
    time::UnixTime,
};
use shoal_hub::test_support::linked;

fn device(name: &str) -> Member {
    Member {
        id: MemberId::generate(),
        kind: MemberKind::Device,
        name: name.into(),
        version: None,
        joined_at: UnixTime::now(),
        last_seen: None,
        connected: true,
        hub: false,
        availability: None,
        agents: Vec::new(),
    }
}

/// The frames the hub answers `member`'s `method` with, a write of `bytes`.
async fn answered(
    manager: &Arc<Manager>,
    member: &Member,
    method: AppMethod,
    bytes: Option<&[u8]>,
) -> Vec<Response> {
    let link = linked();
    let (mut incoming, out) = link.stream();
    let data = bytes.map(|bytes| URL_SAFE_NO_PAD.encode(bytes));
    manager
        .serve_app_request(member, method, data, out)
        .unwrap();
    let mut all = Vec::new();
    while let Some(bytes) = incoming.next().await.unwrap() {
        all.extend(bytes);
    }
    (all.split(|byte| *byte == b'\n'))
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect()
}

fn write(scope: AppScope) -> AppMethod {
    AppMethod::WriteAppEntry {
        app: "quay".parse().unwrap(),
        scope,
        write: Write {
            key: "layout".parse().unwrap(),
            content_type: "application/json".parse().unwrap(),
            replaces: None,
        },
        size: 2,
    }
}

#[tokio::test]
async fn each_device_s_own_entries_on_the_hub_are_its_own_and_shared_ones_everyone_s() {
    let (_root, manager) = test_support::manager().await;
    let (phone, tablet) = (device("phone"), device("tablet"));
    for scope in [AppScope::Device, AppScope::Shared] {
        let first = answered(&manager, &phone, write(scope), Some(b"{}")).await;
        first
            .into_iter()
            .next()
            .unwrap()
            .body
            .into_result()
            .unwrap();
        // The same new entry from the other device: its own is new, the
        // shared one is there already.
        let second = answered(&manager, &tablet, write(scope), Some(b"[]")).await;
        let second = second.into_iter().next().unwrap().body.into_result();
        assert_eq!(second.is_err(), scope == AppScope::Shared, "{second:?}");
    }
}

#[tokio::test]
async fn a_write_without_its_bytes_is_only_checked() {
    let (_root, manager) = test_support::manager().await;
    let phone = device("phone");
    let checked = answered(&manager, &phone, write(AppScope::Shared), None).await;
    let checked = checked.into_iter().next().unwrap().body.into_result();
    assert!(matches!(checked, Ok(Body::Ok)), "{checked:?}");
    answered(&manager, &phone, write(AppScope::Shared), Some(b"{}")).await;
    // The entry exists now, so a write that names no revision is refused
    // before its bytes come.
    let refused = answered(&manager, &phone, write(AppScope::Shared), None).await;
    let refused = refused.into_iter().next().unwrap().body.into_result();
    let refused = refused.unwrap_err();
    let code = (refused.downcast_ref::<RemoteError>()).map(|remote| remote.code.clone());
    assert_eq!(code, Some(ErrorCode::StaleRevision));
}
