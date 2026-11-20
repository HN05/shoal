use shoal_forge_types::items::link::LinkKind;

use super::*;
use crate::stand_in::StandIn;

/// The stand-in with `urls` assigned to the space `id` as issues.
fn assigned(id: &SpaceId, urls: &[&str]) -> StandIn {
    let issues = (urls.iter())
        .map(|url| ItemRef::parse_kind(LinkKind::Issue, url).unwrap())
        .collect();
    StandIn {
        assigned: [(id.clone(), issues)].into(),
    }
}

/// The number of the issue `named` names on `forge`, else of the one
/// assigned to `id`.
async fn linked(daemon: &StandIn, id: &SpaceId, forge: &str, named: Option<&str>) -> Result<u64> {
    let forge = ForgeRepo::parse(forge).unwrap();
    let named = named.map(|named| named.parse().unwrap());
    let issue = named_or_linked(daemon, named, Some(id), &forge, ItemKind::Issue).await?;
    Ok(issue.number().get())
}

#[tokio::test]
async fn a_linked_item_of_an_earlier_origin_is_not_taken_for_the_current_ones() {
    let id = SpaceId::generate();
    let daemon = assigned(&id, &["https://forge.example/team/old/issues/7"]);
    let old = linked(&daemon, &id, "https://forge.example/team/old", None);
    assert_eq!(old.await.unwrap(), 7);
    // After its origin moved to another repository, issue 7 there is another issue.
    let new = linked(&daemon, &id, "https://forge.example/team/new", None);
    assert!(new.await.is_err());
}

#[tokio::test]
async fn several_assigned_issues_need_the_number_of_the_one_acted_on() {
    let id = SpaceId::generate();
    let daemon = assigned(
        &id,
        &[
            "https://forge.example/team/repo/issues/7",
            "https://forge.example/team/repo/issues/8",
        ],
    );
    let forge = "https://forge.example/team/repo";
    let error = linked(&daemon, &id, forge, None).await.unwrap_err();
    assert!(format!("{error:#}").contains("several"), "{error:#}");
    assert_eq!(linked(&daemon, &id, forge, Some("8")).await.unwrap(), 8);
}
