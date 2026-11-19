use std::path::PathBuf;

use shoal_core::update::{Installation, Installed};

use super::{Followed, RETRY, Way, way};

#[tokio::test(start_paused = true)]
async fn a_version_is_acted_on_again_only_while_it_is_not_installed() {
    let followed = Followed::default();
    assert!(followed.take("0.2.0"));
    assert!(!followed.take("0.2.0"));
    // Another version is acted on at once.
    assert!(followed.take("0.3.0"));
    tokio::time::advance(RETRY).await;
    assert!(followed.take("0.3.0"));
    followed.installed("0.3.0");
    tokio::time::advance(RETRY).await;
    assert!(!followed.take("0.3.0"));
}

#[test]
fn only_a_downloaded_release_build_following_the_hub_installs_its_version() {
    let downloaded = || Installed::Downloaded(Installation::Binary(PathBuf::from("/bin/shoal")));
    assert_eq!(
        way(true, false, downloaded()),
        Way::Install(Installation::Binary(PathBuf::from("/bin/shoal")))
    );
    assert!(matches!(
        way(false, false, downloaded()),
        Way::Manual { .. }
    ));
    assert!(matches!(way(true, true, downloaded()), Way::Manual { .. }));
    assert_eq!(way(true, false, Installed::Homebrew), Way::Homebrew);
}
