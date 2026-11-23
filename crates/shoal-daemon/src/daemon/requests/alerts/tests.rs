use super::*;
use crate::daemon::{notifications::AlertKind, test_support::manager};

#[tokio::test]
async fn a_long_notification_listing_goes_in_pages() {
    let (_temp, manager) = manager().await;
    // Distinct notices, as completions handing back long file names make.
    for n in 0..200 {
        manager
            .coordination
            .notify(
                &*manager,
                None,
                AlertKind::SpaceDone,
                format!("assignment finished {n}; results: {}", "x".repeat(700)),
            )
            .await;
    }
    let answer = handle(
        &manager,
        AlertMethod::ListAlerts {
            to: crate::protocol::AlertsFor::User,
            unread_only: true,
            limit: 200,
        },
        None,
    )
    .await
    .unwrap();
    let Answer::Pages(Ok(pages)) = answer else {
        panic!("expected pages");
    };
    assert!(pages.len() > 1, "{} pages", pages.len());
    let listed: usize = pages
        .into_iter()
        .map(|page| {
            Vec::<crate::daemon::notifications::Alert>::try_from(page)
                .unwrap()
                .len()
        })
        .sum();
    assert_eq!(listed, 200);
}
