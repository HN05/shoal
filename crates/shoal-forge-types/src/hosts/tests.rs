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

#[test]
fn pins_outrank_answers_learned_after_them() {
    let _pins = PIN_TESTS.lock().unwrap_or_else(PoisonError::into_inner);
    pin(vec![("Pinned.Answer.Example".into(), ForgeKind::Forgejo)]);
    learn("pinned.answer.example", ForgeKind::GitLab);
    assert_eq!(learned("pinned.answer.example"), Some(ForgeKind::Forgejo));
    pin(Vec::new());
    assert_eq!(learned("pinned.answer.example"), Some(ForgeKind::GitLab));
}
