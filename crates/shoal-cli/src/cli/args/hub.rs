//! Arguments of `shoal hub`.
use clap::{Args, Subcommand};
use shoal_protocol::hub::pairing::PairingCode;

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum HubCommand {
    /// Print a code, with its QR code, that joins one machine or device to the hub.
    ///
    /// It runs on the hub, or on a machine that joined it. The code admits one join within
    /// ten minutes, and carries the hub's URL and its key's fingerprint.
    Pair,
    /// Join this machine to the hub, with a code from shoal hub pair or through a command.
    ///
    /// --connect runs the command, such as "ssh desk shoal daemon connect", whose standard input
    /// and output reach the hub's daemon. That joins without a code, and the command becomes
    /// [machine.hub] connect, this machine's route to the hub.
    Join(JoinArgs),
    /// Leave the hub, revoking this machine's credential there.
    Leave,
    /// List the hub's machines and devices, with its listener.
    Ls,
    /// Install the latest release on the hub, from any machine that joined it.
    ///
    /// The hub's daemon restarts on it once idle, and the machines that follow its version
    /// install it too. A hub that Homebrew installed is refused: run brew upgrade hn05/tap/shoal
    /// there.
    Update,
    /// Revoke the credential of a machine or device, named by its ID or name.
    Revoke { member: String },
}

// Arguments of `shoal hub join`.
#[derive(Debug, Args)]
pub struct JoinArgs {
    /// The code shoal hub pair printed.
    #[arg(required_unless_present = "connect", conflicts_with = "connect")]
    pub code: Option<PairingCode>,
    /// A command whose standard input and output reach the hub's daemon.
    #[arg(long, value_name = "COMMAND")]
    pub connect: Option<String>,
    /// The name this machine takes on the hub; its host name by default.
    #[arg(long)]
    pub name: Option<String>,
}
