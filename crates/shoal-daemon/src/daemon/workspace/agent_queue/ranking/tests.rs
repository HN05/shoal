use rusqlite::params;

use super::*;
use crate::{
    daemon::{
        test_support::manager,
        workspace::agent_queue::tests::{create, register},
    },
    model::Tier,
};

#[tokio::test]
async fn a_space_with_a_pr_it_opened_finishes_until_the_pr_merges() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    let workspace = create(&manager, &repo, "topic").await;
    let pr = "https://forge.example/team/repo/pulls/3";
    let tier = |state: Option<&'static str>| {
        let id = workspace.id.clone();
        manager.store.read(move |tx| {
            tx.execute(
                "INSERT OR IGNORE INTO space_items(space_id,relation,kind,url) VALUES (?1,'opened','pr',?2)",
                params![id, pr],
            )?;
            tx.execute("DELETE FROM pr_activity WHERE space_id=?1", [&id])?;
            if let Some(state) = state {
                tx.execute(
                    "INSERT INTO pr_activity(space_id,url,record) VALUES (?1,?2,json_object('state',?3))",
                    params![id, pr, state],
                )?;
            }
            Ok(spaces::ranked(tx)?[&id].tier)
        })
    };
    // No watch has read the PR yet, so it counts as open.
    assert_eq!(tier(None).await.unwrap(), Tier::Finishing);
    assert_eq!(tier(Some("open")).await.unwrap(), Tier::Finishing);
    assert_eq!(tier(Some("merged")).await.unwrap(), Tier::New);
    // The poll saw it merge while the watch's cursor still says open.
    let id = workspace.id.clone();
    let tier = manager.store.read(move |tx| {
        tx.execute(
            "INSERT INTO space_item_activity(space_id,url,record) VALUES (?1,?2,json_object('state','merged'))",
            params![id, pr],
        )?;
        tx.execute("UPDATE pr_activity SET record=json_object('state','open') WHERE space_id=?1", [&id])?;
        Ok(spaces::ranked(tx)?[&id].tier)
    });
    assert_eq!(tier.await.unwrap(), Tier::New);
}
