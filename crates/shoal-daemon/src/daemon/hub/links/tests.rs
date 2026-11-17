use shoal_ids::member_ids::MemberId;

use super::Links;

#[test]
fn another_installation_cannot_open_a_held_credential() {
    let (links, member) = (Links::default(), MemberId::generate());
    let _first = links.register(&member, "desk").unwrap();
    assert!(links.register(&member, "copy").is_none());
}

#[tokio::test]
async fn the_same_installation_replaces_its_lost_link() {
    let (links, member) = (Links::default(), MemberId::generate());
    let first = links.register(&member, "desk").unwrap();
    let second = links.register(&member, "desk").unwrap();
    first.stopped().await;
    drop(first);
    assert!(links.connected().contains(&member));
    drop(second);
    assert!(links.connected().is_empty());
}
