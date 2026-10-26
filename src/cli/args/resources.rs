//! Arguments of commands for leases and shared resources.
use clap::{Args, Subcommand, ValueEnum};

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum ResourcesCommand {
    /// Acquire a port, simulator, resource permit, or related repository.
    Acquire {
        #[command(subcommand)]
        kind: AcquireKind,
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(long, global = true)]
        workspace: Option<String>,
    },
    /// Release every lease, every lease of one kind, or a selected lease.
    Release {
        #[command(subcommand)]
        kind: Option<ReleaseKind>,
        /// Workspace to use; defaults to the current workspace or picker.
        #[arg(long, global = true)]
        workspace: Option<String>,
    },
    /// Show leases and usage for every kind, or one.
    Leases {
        kind: Option<LeaseKind>,
        /// Workspace to show; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
        /// Show every managed workspace.
        #[arg(long, conflicts_with = "workspace")]
        all: bool,
    },
    /// Show configuration and available inventory for every kind, or one.
    Catalog {
        kind: Option<CatalogKind>,
        /// Workspace whose configuration to show; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
    },
    /// Show recorded requests for a kind.
    History {
        #[command(subcommand)]
        kind: HistoryKind,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CatalogKind {
    /// Automatic range, named ports and conflict policy.
    Port,
    /// Device types, installed runtimes and machine profiles.
    Sim,
    /// Resource pools, their members and approval settings.
    Resource,
    /// Related repositories that can be borrowed.
    Repo,
    /// Agent secrets, the tools they serve and approval settings.
    Secret,
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum HistoryKind {
    /// Review clean-device requests, including failed and busy requests.
    Sim {
        /// Workspace to show; defaults to the current workspace or picker.
        #[arg(long)]
        workspace: Option<String>,
        /// Show every managed workspace.
        #[arg(long, conflicts_with = "workspace")]
        all: bool,
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=50))]
        limit: u32,
        /// Show entries older than this audit ID.
        #[arg(long)]
        before: Option<i64>,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum LeaseKind {
    Hold,
    Port,
    Sim,
    Resource,
    Repo,
}

// Options shared by leases that a workspace may hold several of.
#[derive(Debug, Args)]
pub struct LeaseOptions {
    /// Stable lease name; use different names to hold several leases.
    #[arg(long, default_value = "default")]
    pub name: String,
    /// Explain what the lease is used for.
    #[arg(long)]
    pub reason: Option<String>,
    /// Queue for capacity and wait until acquired or denied.
    #[arg(long)]
    pub queue: bool,
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum AcquireKind {
    /// Keep a workspace against automatic cleanup until released.
    Hold {
        #[arg(long)]
        name: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Reserve a named TCP port, or return the existing reservation.
    Port {
        /// Port name; omit to choose a configured port.
        name: Option<String>,
        #[arg(long)]
        port: Option<u16>,
        /// Environment variable exported to subsequent workspace commands.
        #[arg(long)]
        env: Option<String>,
        /// Explain what the reservation is used for.
        #[arg(long)]
        reason: Option<String>,
        /// Override the repo's conflict behavior (default: suggest).
        #[arg(long, value_enum)]
        on_conflict: Option<crate::config::repo::ConflictPolicy>,
    },
    /// Lease an Xcode simulator; repeated requests return the same named lease.
    Sim {
        #[command(flatten)]
        lease: LeaseOptions,
        /// Configured simulator profile.
        #[arg(long, conflicts_with_all = ["device", "runtime"])]
        profile: Option<String>,
        /// Device type name or identifier.
        #[arg(long)]
        device: Option<String>,
        /// Installed runtime; defaults to the latest compatible iOS runtime.
        #[arg(long, requires = "device")]
        runtime: Option<String>,
        /// Require a fresh or erased device; a reason is mandatory and audited.
        #[arg(long, requires = "reason")]
        clean: bool,
    },
    /// Acquire a permit or reader/writer lock from a configured pool.
    Resource {
        /// Configured resource or pool; omit to choose one.
        pool: Option<String>,
        /// Pool member to acquire instead of any available one.
        #[arg(long)]
        member: Option<String>,
        /// Lock mode (defaults to permit for semaphores, write for rwlocks).
        #[arg(long, value_enum)]
        mode: Option<crate::daemon::resources::LockMode>,
        #[command(flatten)]
        lease: LeaseOptions,
    },
    /// Borrow a configured related repository's checkout for reading.
    Repo {
        /// Configured `kind = "repo"` resource name; omit to choose one.
        #[arg(value_name = "NAME")]
        resource: Option<String>,
        #[command(flatten)]
        lease: LeaseOptions,
    },
    /// Request access to a configured secret that requires approval.
    Secret {
        /// Variable name of the secret in `[secrets]`.
        name: String,
        /// Explain what the secret is needed for.
        #[arg(long)]
        reason: Option<String>,
        /// Wait until approved or denied.
        #[arg(long)]
        queue: bool,
    },
}

// A pool, optionally narrowed to one named lease.
#[derive(Debug, Args)]
pub struct PoolSelection {
    /// Configured resource or pool; omit to release every lease of this kind.
    #[arg(value_name = "NAME")]
    pub pool: Option<String>,
    /// Release only this lease name.
    #[arg(long = "name", value_name = "LEASE", requires = "pool")]
    pub lease: Option<String>,
}

#[derive(Debug, Subcommand)]
#[command(defer = true)]
pub enum ReleaseKind {
    /// Release a named workspace hold; omit its name to choose one.
    Hold {
        #[arg(long)]
        name: Option<String>,
    },
    /// Release a port reservation, or every one.
    Port { name: Option<String> },
    /// Release a simulator lease, or every one.
    Sim {
        #[arg(long)]
        name: Option<String>,
    },
    /// Release resource permits and locks.
    Resource {
        #[command(flatten)]
        selection: PoolSelection,
    },
    /// Release related repository leases.
    Repo {
        #[command(flatten)]
        selection: PoolSelection,
    },
    /// End a secret grant or cancel its request, or every one.
    Secret { name: Option<String> },
}
