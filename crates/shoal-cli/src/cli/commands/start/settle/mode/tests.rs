use super::ensure_fits;
use shoal_protocol::launches::Launch;

/// A named mode refuses what its kind of space cannot take, such as a
/// branch for a chat, before a picker asks for a repository.
#[test]
fn a_chat_mode_refuses_a_branch_before_any_picker() {
    let settings = crate::config::Config::default()
        .resolve(&Default::default())
        .unwrap();
    let modes = crate::config::modes::shapes(&settings.modes);
    let launch = Launch {
        branch: Some("fix".into()),
        ..Launch::default()
    };
    let error = ensure_fits(&launch, &"chat".into(), &modes).unwrap_err();
    assert!(error.to_string().starts_with("--branch "), "{error}");
    ensure_fits(&launch, &"worker".into(), &modes).unwrap();
}
