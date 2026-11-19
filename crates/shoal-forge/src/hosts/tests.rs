use std::sync::{Mutex, PoisonError};

use super::*;
use crate::RepoRequests;
use shoal_forge_types::hosts::detect;
use shoal_forge_types::hosts::learn;
use shoal_forge_types::hosts::learned;
use shoal_forge_types::hosts::pin;

/// Serializes the tests that replace this process's pins.
static PIN_TESTS: Mutex<()> = Mutex::new(());

/// Every host resolves as `0`; `1` drops the pins first, as a reload
/// would while the host resolves.
struct Resolved(HostType, bool);

impl HostTypes for Resolved {
    async fn forge_type(&self, _host: &str, _scheme: &'static str) -> Result<HostType> {
        if self.1 {
            pin(Vec::new());
        }
        Ok(self.0)
    }
}

#[test]
fn resolving_a_pinned_host_keeps_its_answer_for_after_the_pin() {
    let _pins = PIN_TESTS.lock().unwrap_or_else(PoisonError::into_inner);
    let remote = "git@resolved.pinned.example:team/repo.git";
    let resolve = |resolved| {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        runtime
            .block_on(ForgeRepo::resolve(remote, &resolved))
            .unwrap();
    };
    resolve(Resolved(HostType::Answered(ForgeKind::GitLab), false));
    pin(vec![("resolved.pinned.example".into(), ForgeKind::Forgejo)]);
    // The pin goes while the host resolves as it.
    resolve(Resolved(HostType::Settled(ForgeKind::Forgejo), true));
    assert_eq!(learned("resolved.pinned.example"), Some(ForgeKind::GitLab));
}

#[test]
fn learned_hosts_override_names_and_guesses() {
    assert_eq!(detect("ghe.learned.example", None), ForgeKind::Forgejo);
    learn("GHE.learned.example", ForgeKind::GitHub);
    assert_eq!(detect("ghe.learned.example", None), ForgeKind::GitHub);
    let repo = ForgeRepo::parse("git@ghe.learned.example:team/repo.git").unwrap();
    assert_eq!(
        repo.pull("3").unwrap().1,
        "https://ghe.learned.example/team/repo/pull/3"
    );
}
