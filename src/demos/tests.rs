use super::*;

fn demo(cmd: &[&str]) -> Demo {
    Demo {
        cmd: cmd.iter().map(|arg| (*arg).to_owned()).collect(),
        preset: None,
        open: None,
        scheme: None,
        ports: None,
    }
}

#[test]
fn names_follow_port_names_and_leave_subcommands_alone() {
    assert!(validate_name("web").is_ok());
    assert!(validate_name("Web").is_err());
    assert!(validate_name("ls").is_err());
    assert!(validate_name("start").is_err());
}

#[test]
fn commands_need_an_executable() {
    assert!(demo(&["npm", "run", "dev"]).validate().is_ok());
    assert!(demo(&[]).validate().is_err());
    assert!(demo(&[" "]).validate().is_err());
    let mut open = demo(&["server"]);
    open.open = Some(Vec::new());
    assert!(open.validate().is_err());
    let mut ports = demo(&["server"]);
    ports.ports = Some(vec!["Web".into()]);
    assert!(ports.validate().is_err());
    let mut https = demo(&["server"]);
    https.scheme = Some(Scheme::Https);
    assert!(https.validate().is_err());
    https.preset = Some(Preset::Web);
    assert!(https.validate().is_ok());
}
