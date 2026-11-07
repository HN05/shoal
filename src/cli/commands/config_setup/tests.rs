use std::path::Path;

use super::{installed_config, subject};
use crate::{cli::context::Context, config::Config, paths::Paths};

#[test]
fn setup_installs_the_default_config_only_where_there_is_none() {
    let home = tempfile::tempdir().unwrap();
    let ctx = Context::new(Paths::for_test(home.path()), true);
    let file = Config::path(&ctx.paths);
    let installed = installed_config(&ctx).unwrap();
    assert_eq!(installed, std::fs::canonicalize(&file).unwrap());
    let packaged = crate::config::PACKAGED
        .iter()
        .find(|(name, _)| *name == "default")
        .map(|(_, text)| *text);
    assert_eq!(std::fs::read_to_string(&file).ok().as_deref(), packaged);
    std::fs::write(&file, "# mine\n").unwrap();
    installed_config(&ctx).unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "# mine\n");
}

#[test]
fn the_subject_names_the_global_config() {
    let subject = subject(Path::new("/home/me/.config/shoal/config.toml"));
    assert!(
        subject.contains("`/home/me/.config/shoal/config.toml`"),
        "{subject}"
    );
}
