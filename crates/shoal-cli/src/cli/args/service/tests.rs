use clap::Parser as _;

use super::*;

#[test]
fn stopping_the_daemon_waits_unless_told_to_refuse_or_force() {
    let when = |flags: &[&str]| {
        let line = ["shoal", "daemon", "restart"].iter().chain(flags);
        let cli = crate::cli::Cli::try_parse_from(line)?;
        let Some(crate::cli::Command::Service(ServiceCommand::Daemon {
            command: DaemonCommand::Restart(wait),
        })) = cli.command
        else {
            panic!("expected daemon restart");
        };
        Ok::<_, clap::Error>(wait.when())
    };
    assert_eq!(when(&[]).unwrap(), ExitWhen::Idle);
    assert_eq!(when(&["--no-wait"]).unwrap(), ExitWhen::IfIdle);
    assert_eq!(when(&["--force"]).unwrap(), ExitWhen::Now);
}
