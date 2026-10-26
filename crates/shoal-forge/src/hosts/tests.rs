use super::*;

/// Serializes the tests that replace this process's pins.
static PIN_TESTS: Mutex<()> = Mutex::new(());

#[test]
fn readers_wait_for_the_pins_another_reader_is_loading() {
    let _pins = PIN_TESTS.lock().unwrap_or_else(PoisonError::into_inner);
    let (loading, started) = std::sync::mpsc::channel();
    let (proceed, waiting) = std::sync::mpsc::channel::<()>();
    learn_pins(move || {
        loading.send(()).unwrap();
        waiting.recv().unwrap();
        vec![("gitlab.pinned.example".into(), ForgeKind::Forgejo)]
    });
    let first = std::thread::spawn(|| learned("other.pinned.example"));
    started.recv().unwrap();
    // Any other reader would wait for the pins.
    assert!(PINS.try_lock().is_err());
    proceed.send(()).unwrap();
    assert_eq!(first.join().unwrap(), None);
    assert_eq!(
        ForgeKind::detect("gitlab.pinned.example", None),
        ForgeKind::Forgejo
    );
}

#[test]
fn pins_set_outright_replace_the_ones_yet_to_load() {
    let _pins = PIN_TESTS.lock().unwrap_or_else(PoisonError::into_inner);
    // A reader in another test may load these before the pins below
    // replace them, so they must not fail it; they only show if `pin`
    // keeps them pending.
    learn_pins(|| vec![("outright.pinned.example".into(), ForgeKind::GitLab)]);
    pin(vec![("outright.pinned.example".into(), ForgeKind::Forgejo)]);
    assert_eq!(learned("outright.pinned.example"), Some(ForgeKind::Forgejo));
}

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
fn pins_outrank_answers_learned_after_them() {
    let _pins = PIN_TESTS.lock().unwrap_or_else(PoisonError::into_inner);
    pin(vec![("Pinned.Answer.Example".into(), ForgeKind::Forgejo)]);
    learn("pinned.answer.example", ForgeKind::GitLab);
    assert_eq!(learned("pinned.answer.example"), Some(ForgeKind::Forgejo));
    pin(Vec::new());
    assert_eq!(learned("pinned.answer.example"), Some(ForgeKind::GitLab));
}

#[test]
fn learned_hosts_override_names_and_guesses() {
    assert_eq!(
        ForgeKind::detect("ghe.learned.example", None),
        ForgeKind::Forgejo
    );
    learn("GHE.learned.example", ForgeKind::GitHub);
    assert_eq!(
        ForgeKind::detect("ghe.learned.example", None),
        ForgeKind::GitHub
    );
    let repo = ForgeRepo::parse("git@ghe.learned.example:team/repo.git").unwrap();
    assert_eq!(
        repo.pull("3").unwrap().1,
        "https://ghe.learned.example/team/repo/pull/3"
    );
}
