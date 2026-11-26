//! The `shoal` command: it runs [`shoal_cli`], passing it the daemon for
//! `shoal daemon run`.

fn main() {
    shoal_cli::run(|paths, managed, handoff| Box::pin(shoal_daemon::run(paths, managed, handoff)));
}
