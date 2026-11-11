//! The `shoal` command: the CLI of [`shoal_cli`], which runs the daemon of
//! [`shoal_daemon`] for `shoal daemon run`.

fn main() {
    shoal_cli::run(|paths, managed, handoff| Box::pin(shoal_daemon::run(paths, managed, handoff)));
}
