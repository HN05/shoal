# Overview

This file states Shoal's purpose and boundaries, its core terms, the
principles of its architecture and the work that remains.

## Purpose

Shoal manages local Git workspaces, the processes that run in them and the
development resources they share, for people and coding agents.

Cleanup is part of owning a resource. A disposable workspace leaves no
substantial data of its runs behind. [Cleanup](cleanup.md) says how.

## Core terms

- A **space** is a unit of work that Shoal keeps: a workspace or a chat.
  Spaces form a tree, and each space has at most one parent.
- A **workspace** is a space with a Git worktree of its own, usually on a
  branch of its own.
- A **chat** is a space without a worktree of its own. It runs in a
  repository's registered checkout, in a workspace's worktree, or without a
  repository, as [Chats](chats.md) says.
- A caller's **scope** is the spaces it may act on: its own space and the
  spaces below it. A [master session](chats.md#master-session)'s scope reaches
  every space. Only the user's own terminal is unscoped.
  [Scope](permissions.md#scope) says how a scope is given and checked.
- A space's **coordinator** is the agent of the nearest space above it that
  has an agent. The space's alerts, threads and completion reach its
  coordinator, or the user when it has none.

## Boundaries

### Scope is cooperative

Agents cooperate with Shoal's scope and with the resources it allocates.
Shoal does not stop a hostile process of the same user from going around
them, through Git, the filesystem or a resource itself.
[Filesystem restrictions](#filesystem-restrictions) are planned.

### What Shoal leaves to others

- Shoal does not provision tools, manage browsers or store conversations.
- Shoal never switches the user's login, or the daemon's.
- Terminal hosts own the terminals. Shoal opens places through them, as
  [Terminal hosts](terminals.md) says.
- An integration, such as an agent tool, a terminal host or an app, adds only
  what it does differently. State and decisions stay in Shoal's generic code.
  [Configuration and integrations](agents.md#configuration-and-integrations)
  shows this for agent tools.
- A workspace's service containers run beside it, one Compose project per
  workspace. The runtime's `compose` reads the Compose file, and Shoal never
  parses it. The agent and its tools stay on the host.
- Shoal exposes no listener to the internet. The user puts a tunnel of their
  choice in front of one.

### Each machine schedules its own work

A machine's agent queue admits its agents, and the CI jobs it runs for
registered repositories. The agents and jobs thus share one admission instead
of competing for memory. [Agent queue](queue.md) and
[Runner and triggers](runner.md) say how.

A trigger starts an agent when its source fires, such as a timer or a webhook
request.

### When Shoal refuses a request

Shoal refuses a request only for one of these reasons:

- it would act outside the caller's scope or permissions,
- it would lose or damage work,
- it cannot be done at all.

A limit of how Shoal is built is no such reason, such as a code path that one
kind of caller does not take. Neither is a view of where a setting or action
belongs, or of what the user ought to want. Shoal supports such a request
instead. For example, a config file takes every option its schema has, as
[Configuration](config.md#one-schema-for-every-file) says.

## Architecture

### One binary and one daemon

One Rust binary holds the CLI, the execution wrapper and the daemon. Each
state directory has one daemon, which every repository shares. The ordinary
installation is per user. [Daemon](daemon.md) and [Execution](execution.md)
cover each.

### Protocol

The daemon listens on a private Unix socket and speaks a versioned JSON
protocol. The CLI and apps use the same protocol, through the `shoal-client`
library. [Apps](../reference/apps.md) says how an app takes the library.

The protocol keeps no compatibility promise, as
[Compatibility](../../AGENTS.md#compatibility) says: a daemon refuses a client
of another release.

### Crates

The crates split Shoal by who runs the code, so a change rebuilds only the
code that uses it. The CLI and the daemon share `shoal-core` and never depend
on each other. The daemon is `shoal-daemon`, over `shoal-store`,
`shoal-daemon-base` and a crate for each subsystem.
[Crates](../../AGENTS.md#crates) states the rules the crates follow, and each
crate's `//!` names its responsibility.

### Subsystems

Each subsystem of the daemon is a crate. What it needs of the others is an
interface it declares, which the daemon implements, mostly by calling another
subsystem. Each subsystem thus builds and tests alone. Its design file says
what its crate holds and what it leaves to the daemon.
[Composition](daemon.md#composition) says what the daemon keeps.

## Remaining work

Filesystem restrictions come next. These items also remain open:

- **Storage policy:** who owns run data outside the worktree, and how long it
  stays. Temporary directories, caches, logs, alerts, kept branches and audit
  history already have a policy.
- **Distribution:** a service identity that stays stable across upgrades, and
  validation of the native Linux service and its recovery.
- **Execution environments:** coordination between host and guest, and
  between users. Separate state directories have separate capacity.

### Filesystem restrictions

This policy is decided and not implemented.

- Shoal gives cooperative agents guardrails, and offers an explicit
  unrestricted mode.
- Read access and write access are separate.
- Precedence runs global deny, global allow, repository grant, then deny.
  Global denies win inside allowed directories.
- No rule grants the whole home directory implicitly.
- An inspectable baseline covers the worktree, temporary storage, runtime
  dependencies, shared Git metadata and tool state.
- Shoal resolves symlinks. Writable access is separate from cleanup
  ownership.
- An unsupported policy fails clearly instead of launching unrestricted.
- The launcher applies restrictions, not the daemon. Seatbelt is the intended
  macOS backend. Landlock is a Linux candidate, pending nested allow and deny
  rules, with bubblewrap as the fallback.
