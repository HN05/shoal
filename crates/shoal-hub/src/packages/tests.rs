//! What the tests of packages share: a hub with machines, their reports
//! and the packages they send.
use std::sync::Arc;

use anyhow::Result;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rusqlite::params;
use serde_json::json;
use shoal_core::config::Config;
use shoal_ids::{member_ids::MemberId, space_ids::SpaceId};
use shoal_protocol::hub::directory::{Report, SpaceSummary};

use super::Sent;
use crate::{
    Hub,
    stand_in::{self, StandIn},
    streams::{Incoming, Outgoing},
    test_support::linked,
    time::UnixTime,
};

/// A hub, on `config`, that records the machines `members`.
pub(crate) async fn hub(root: &tempfile::TempDir, config: &str, members: &[&str]) -> Arc<Hub> {
    let hub = stand_in::hub(root, Config::from_toml(config).unwrap()).await;
    std::fs::create_dir_all(&hub.paths.state).unwrap();
    for (index, member) in members.iter().enumerate() {
        let member = MemberId::from((*member).to_owned());
        let key = URL_SAFE_NO_PAD.encode([index as u8; 32]);
        (hub.store)
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO hub_members(id, kind, name, public_key, joined_at)
                     VALUES (?1,'machine',?1,?2,0)",
                    params![member, key],
                )?;
                Ok(())
            })
            .await
            .unwrap();
    }
    hub
}

fn summary(id: &str) -> SpaceSummary {
    serde_json::from_value(json!({
        "id": id, "number": 1, "name": id, "kind": "chat", "status": "working",
    }))
    .unwrap()
}

/// Record `member`'s report that it has the spaces `ids`.
pub(crate) async fn report(hub: &Hub, member: &str, ids: &[&str]) {
    let spaces = ids.iter().map(|id| summary(id)).collect();
    let member = MemberId::from(member.to_owned());
    (hub.record_report(
        &StandIn::default(),
        &member,
        Report::Whole(spaces),
        Vec::new(),
    ))
    .await
    .unwrap();
}

/// What the hub answers `member` sending `bytes` as its package of `space`,
/// once the hub ends the stream.
pub(crate) async fn send(hub: &Arc<Hub>, member: &str, space: &str, bytes: &[u8]) -> Result<()> {
    let link = linked();
    let (mut answer, mut package): (Incoming, Outgoing) = link.opener.streams().open_duplex();
    let sent = Sent {
        member: MemberId::from(member.to_owned()),
        space: SpaceId::from(space.to_owned()),
        built_at: UnixTime::now(),
    };
    let streams = (link.writer.streams(), answer.number());
    hub.keep_package(&StandIn::default(), sent, streams).await?;
    package.write(bytes).await.unwrap();
    package.end(None).await;
    while answer.next().await?.is_some() {}
    Ok(())
}

pub(crate) fn kept(hub: &Hub, space: &str) -> Option<Vec<u8>> {
    std::fs::read(hub.package_path(&SpaceId::from(space.to_owned()))).ok()
}
