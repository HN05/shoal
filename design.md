# Shoal design

Living design document. Confirmed decisions reflect the discussion with the user;
proposals and open questions are not yet commitments.

## Purpose

Shoal is a CLI for orchestrating agents on the same machine. Both humans and
orchestrator agents use it to start and manage workers.

Shoal is expected to be used with Superlogical. Details of that project and its
integration are limited, so the initial focus is resource allocation. The working
assumption is that Superlogical behaves roughly like tmux; its exact interface
and guarantees have not been verified.

## Related tools and execution environments

The user is also building [Macraft](https://git.henriknordvik.com/HN05/Macraft), a
macOS tool that can spin up Podman containers, Tart macOS VMs, and Apple containers.
The intent is to include Shoal in Macraft in some form, together with
Superlogical, to make agentic development easy. This context comes from the user;
the repository contents have not been reviewed.

Both humans and orchestrator agents are intended callers of the Shoal CLI. A
concrete command shape has not yet been selected.

The relationship between these tools is not yet a settled architecture:

- Macraft provides the context for provisioning containers and VMs.
- Shoal owns agent lifecycles and coordinates resources, with resource allocation
  as its initial focus.
- Superlogical is assumed to provide terminal/session management, roughly like
  tmux. Its exact responsibilities and interface remain to be defined.

### Integration boundary with Superlogical

Shoal must not depend on or contain code specific to Superlogical. Superlogical
calls Shoal through a generic CLI interface and owns its session integration.

The intended workflow is:

1. A human or orchestrator requests an agent from a master Superlogical session.
2. Superlogical calls Shoal to prepare a run: its Git worktree, working directory,
   environment, filesystem policy, and any resources needed at startup.
3. Shoal returns a run identifier and the information needed to launch within
   that run. Exact commands and output fields remain to be designed.
4. Superlogical creates a new session in the prepared directory and starts the
   agent through Shoal's generic execution interface. This interface must apply
   the run's environment and filesystem policy and track its processes; its exact
   implementation remains open.
5. When that Superlogical session is deleted, Superlogical calls Shoal to remove
   the associated run. Shoal stops remaining run-owned processes, releases
   resources and ports, and cleans up disposable data under the retention policy.

Superlogical owns the mapping from session identifiers to Shoal run identifiers.
Shoal knows about its runs, processes, resources, and storage, not Superlogical
sessions. Other callers can use the same interface.

Under the tmux-like assumption, Shoal can leave the following to Superlogical:

- Terminal rendering, pseudo-terminal (PTY) hosting, panes, windows, and layouts.
- Interactive input routing, keyboard shortcuts, and switching between agents.
- Attaching to and detaching from terminal sessions, and keeping those sessions
  available when the user disconnects, if Superlogical provides that guarantee.
- Terminal scrollback and a terminal-oriented agent dashboard.

Shoal does not need to implement a terminal multiplexer or duplicate those
features. A tmux-like role alone does not imply durable log storage, conversation
resumption, task planning, or recovery after a machine reboot.

Shoal still owns:

- Run identity, worktree and environment setup, filesystem permissions, and
  execution of agent startup, stop, and removal requests.
- Simulator lifecycle, resource allocation and waiting, and port
  reservations.
- Tracking run-owned processes and storage, cleanup, and allocation recovery
  when processes exit or a caller requests removal.
- Machine-readable run/resource status, errors, and lifecycle notifications for
  integrations. The presentation of these belongs to the calling tool.

#### Proposed failure and cleanup behavior

- If Superlogical cannot create the session after preparation, it calls Shoal to
  remove the prepared run. Shoal should also clean up partially failed setup.
- Removal should be retryable, so duplicate calls or interrupted cleanup are
  harmless. Resources should not be reused until their former use has stopped.
- Detaching from a session should not trigger removal. Session deletion is the
  intended cleanup trigger; deciding when an agent's task is complete is separate.
- A missed deletion notification or crashed caller needs a generic recovery
  mechanism. Run listing/reconciliation or leases are candidates; no mechanism
  has been chosen. Shoal must not inspect Superlogical sessions to recover.
- Direct CLI callers should be able to use the same prepare, execute, status,
  and removal operations without Superlogical.

Task decomposition, model routing, conversation history, and semantic task
completion are not required by Shoal's initial resource-allocation focus. They
may belong to an orchestrator, but the tmux analogy does not establish that
Superlogical provides them.

The dependency direction and deletion-triggered cleanup are the intended design.
The exact Superlogical capabilities, CLI contract, and failure recovery mechanisms
have not yet been verified or finalized.

### Open integration decisions

- Does Shoal run on the host, inside each VM/container, or in both places?
- Does each environment manage an independent resource inventory, or must guest
  agents also reserve resources on the host or in another environment?
- Does Macraft provision an environment before invoking Shoal, or can
  Shoal request provisioning through an integration?
- How are guest service ports exposed on the host, and which tool owns allocating
  host ports and managing their mappings?
- What generic CLI commands and structured output do callers need to prepare,
  execute, inspect, and remove Shoal runs?

These questions do not add VM/container provisioning or cross-environment
coordination to Shoal's confirmed initial scope.

Macraft may also create a container and automatically open a Superlogical session
without using Shoal. Shoal is needed when the caller wants its managed runs and
resource allocation. Macraft owns container removal; Shoal owns removal of runs
it manages within the environment.

## Confirmed direction

- Implement Shoal in Rust.
- Build the CLI and local daemon first. Provide `shoal setup` to configure
  the daemon with the operating system. Follow the implementation order below;
  advanced process recovery and filesystem restrictions come late.
- Shoal has global machine configuration. Additional filesystem access defaults
  to what the repository declares, with global always-allow and never-allow paths
  for read and write access. Global denies take precedence.
- Shoal accepts a repository URL or a local repository path.
- Build tools and system prerequisites are the machine's responsibility. Shoal
  does not provision them through Homebrew or another system package manager.
- Shoal owns the whole agent lifecycle: it creates the agent's Git worktree,
  working directory, and environment, starts the agent, and cleans up afterward.
- Shoal is independent of Superlogical. Superlogical calls Shoal, creates the
  terminal session for a prepared run, and requests run cleanup on session
  deletion. Shoal exposes generic operations and does not manage those sessions.
- Agents often work on the same project on different branches.
- Most agents should be easy to remove cleanly. Removing an agent through Shoal
  must not leave large files or large numbers of files behind, except for caches
  and similar reusable data. Exact retention policies remain to be defined.
- Shoal coordinates access to shared resources, including Xcode simulators.
  Multiple instances of a resource can exist; allocation must account
  for individual instances rather than assume a single global resource.
- Shoal supports exclusive access to shared resources when needed.
- Shoal boots and shuts down simulators. It owns
  their runtime lifecycle as well as their allocation.
- Simulator configuration specifies which simulators agents may use and how
  many may run simultaneously. An alternative mode permits agents to request
  any simulator when necessary. Shoal should choose intelligently which idle
  simulators to shut down as resource needs change.
- Simulator requests use only installed runtimes. Shoal does not download
  missing runtimes, including in the "any simulator" mode.
- Use `simctl` for simulator creation, boot, shutdown, and deletion. Evaluate
  `devicectl` for additional interactions.
- Shoal does not manage browsers.
- Shoal reserves ports so agents and their applications do not conflict.
- Agents are assumed to follow instructions and honor resource and port
  allocations. Enforcing their use against bypasses is outside the current scope
  and may be considered in the future.
- Filesystem isolation should provide practical guardrails for cooperative
  agents. A separate filesystem view or a complete read-only boundary covering
  every filesystem operation is not required.
- The intended macOS filesystem sandbox uses Seatbelt. Linux needs an equivalent
  mechanism for restricting filesystem access; the mechanism is undecided.
- An unrestricted execution mode is also needed, especially for use inside VMs
  and containers.

These describe the overall product direction. Deliver them incrementally in the
implementation order below, starting with the CLI and daemon foundation.

## Repository input and project configuration

### Confirmed boundary

Shoal accepts a repository URL or local path and prepares the run's workspace.
Installing build tools and other machine prerequisites belongs to machine setup,
which may be performed by the user or environment tooling. The earlier idea of
Shoal installing these through Homebrew is superseded by this boundary.

Project dependencies are a separate question: restoring packages inside a fresh
worktree may still be necessary even when the machine has all required tools.
Shoal supports an optional workspace setup command for automatic dependency
restoration using the project's existing tools.

### Confirmed repository configuration

No mandatory new repository dependency manifest is needed. Existing project
manifests and lockfiles remain authoritative for language/package dependencies.
Shoal should not duplicate them or implement its own dependency resolver.

An optional Shoal-specific repository configuration supports:

- Preferred simulators and resource requirements.
- Named ports and environment variables.
- Allowed filesystem paths, distinguishing read-only access from writable access
  for project-specific tool needs, within machine policy.
- A workspace setup command for automatic dependency restoration when desired.
  The command uses the project's existing package manager and lockfiles.

These capabilities and the configuration locations below are confirmed; the
schema and serialization format are undecided. Filesystem precedence is defined
below. Caller overrides and
precedence for other configuration settings remain to be designed.

### Configuration locations and format

**Confirmed:** look for repository configuration in both locations, relative
to the selected worktree root:

- `.shoal.toml`
- `.shoal/config.toml`

The two layouts serve different repository needs:

- Use `.shoal.toml` for standalone configuration when no supporting scripts
  are needed.
- Use `.shoal/config.toml` when keeping supporting scripts alongside the
  configuration in `.shoal/`, for example `.shoal/setup.sh`.

Scripts are repository-owned files referenced by configuration. Their presence
alone does not cause Shoal to execute them; command syntax and script-path
resolution remain to be specified with the configuration schema.

Behavior when both files exist remains undecided: precedence, merging, or an
explicit ambiguity error must be specified. Keep daemon state and allocations
outside the repository.

**Confirmed global location:** `~/.config/shoal/config.toml`. Respecting
`XDG_CONFIG_HOME` as an absolute-path override is implemented. Global filesystem policy
retains its established precedence over repository grants.

**The repository configuration file format and schema are not yet set.** Global
configuration now uses TOML for `[auto_cleanup]` (`enabled = true`,
`idle_minutes = 10` by default). Repository setup-command syntax, resource
requirements, and port/environment mapping syntax remain undecided.

Read the configuration from the selected worktree so branches can carry their
own settings. Proposed validation errors should identify the file and setting
involved. Configuration update behavior remains open.

### Global machine configuration

Confirmed: Shoal provides global configuration for machine policy. Filesystem
read and write access are evaluated separately using these rules, in order:

1. **Never allow:** a matching global deny blocks that access even if the repo
   requests it or a global allow also matches.
2. **Always allow:** a matching global allow grants that access without requiring
   a repository declaration.
3. **Repo-declared:** otherwise, extra access is granted only if declared by the
   repository configuration. A second global allow entry is not required.
4. **Undeclared:** extra access is denied.

The default therefore does not grant broad read access to the home directory.
For example, global read access to a tool-config directory can make it available
to all repos; a repo can request writes to a project cache; a global write deny
on that cache wins over the repo request. Directory rules should cover their
descendants, with denies winning over more specific allows as well.

Shoal also needs baseline access for the run worktree, runtime dependencies, and
temporary storage. The proposed baseline must be explicit and inspectable, not
a hidden grant to the whole home directory. Its exact paths are undecided. A
conflict with a global deny should fail preparation with a clear explanation,
rather than override the deny.

Portable path syntax and whether machine policy may disable unrestricted mode
remain open. In unrestricted mode no filesystem
sandbox is applied; it must not be presented as enforcing these path rules.

#### Allowed path configuration

Repository configuration can declare extra paths needed by project tools, such
as a shared cache or tool-specific state directory. Distinguish read and write
access according to global policy. Repo declarations grant additional access by
default unless a matching global deny prohibits it.

Proposed rules:

- Resolve repository-relative paths against the prepared worktree. Provide an
  explicit way to refer to the user's home directory and machine-defined cache
  locations without hardcoding a developer's absolute path. Syntax is undecided.
- Apply global denies and allows before repository declarations. Report a repo
  request that conflicts with a global deny.
- Use the effective policy for setup commands and agent execution. Tool launch
  profiles may contribute their own known config/state paths under the same
  machine limits.
- Canonicalize existing paths and account for symlinks and directories created
  during setup when deriving the backend policy.
- Permission and ownership are separate: a writable shared directory must not
  be deleted on run removal just because the repo declared access to it. Cleanup
  applies to tracked run-owned data; shared caches retain their separate policy.

Path validation and handling of missing paths remain to be defined.

Without project configuration, the proposed baseline is to prepare the workspace
using machine/caller defaults and allow resources to be requested dynamically.
Do not guess and execute install commands just because a manifest is present.
Setup command timing, execution policy, filesystem access, retry behavior, and
failure cleanup must be defined. Shared caches may be retained;
run-local dependency directories follow ordinary run cleanup.

### Open repository decisions

- How does a caller select the starting branch or commit?
- For a local repository, is the starting point committed history only, or may
  the caller explicitly include uncommitted changes?
- How are fetched repositories cached and retained between runs?
- When should the configured setup command run, and when is a workspace reported
  ready? How are setup failures and retries exposed to callers?
- What configuration format should v1 use? How do named
  ports map to environment variables, including during setup?

## Initial focus: resource allocation

### Required capabilities

- Track available shared resource instances.
- Allocate exclusive access to an instance and serialize competing requests.
- Support multiple available instances, such as several simulators.
- Start and stop managed simulator instances.
- Reserve ports for agents working concurrently, including on different branches
  of the same project.
- Relate allocations to their owners so they can be cleaned up with the agent
  lifecycle. Exact ownership and recovery rules are still undecided.

### Simulator lifecycle

Shoal is responsible for booting and shutting down simulators. Allocation
therefore includes management of running instances.

Startup and shutdown ownership is confirmed. Policies for creating simulator
devices, resetting their state, reusing running instances,
and choosing when to shut down idle instances remain open.

### Simulator configuration and scheduling

#### Confirmed requirements

- Support a configured set of simulators that agents are allowed to use.
- Support an alternative policy allowing any simulator when necessary. Agents
  should avoid requesting additional or different simulators without a task need.
- Only installed runtimes may be used; Shoal does not download missing runtimes.
- Configure the maximum number of simultaneously running simulators.
- Manage shutdown decisions intelligently when making capacity available.
- Use `simctl` for creation, boot, shutdown, and deletion; evaluate `devicectl`
  for additional interactions.

The configuration format, default limit, and scope of configuration (machine,
project, or run) have not been selected. Allowing any simulator does not imply
unlimited concurrency and is restricted to installed runtimes. Shoal may create
a simulator device using an installed runtime under the configured policy.

#### Proposed scheduling policy

1. Match each request against the allowed devices and OS versions. Reject a
   disallowed request with a clear explanation; do not silently substitute an
   incompatible device or runtime. If its required runtime is not installed,
   return an actionable error instead of downloading it or waiting for capacity.
2. Prefer a compatible, unallocated running instance when its state can be safely
   reused. Otherwise, boot an existing compatible stopped instance or create one
   as needed under the configured policy.
3. Reserve capacity before starting a boot so concurrent requests cannot each
   consume the same last slot. Count booting and running instances against the
   limit, and keep a shutting-down instance counted until shutdown is confirmed.
4. At capacity, reclaim only Shoal-managed instances that have no active
   allocation. Prefer idle instances not needed by queued requests, then the
   least recently used. Never interrupt an active allocation to make room.
5. If every slot is actively allocated, wait or return a capacity error according
   to the caller's waiting policy. Timeout behavior remains to be decided.
6. Consider a configurable idle grace period to avoid repeated shutdown/boot
   cycles. Release and shutdown are separate from deleting a device's stored
   data; the cleanup policy still determines what is retained.

This is a proposed algorithm. Idle means no active allocation, not low CPU usage
or a quiet screen. The exact treatment of external simulators and devices
created indirectly by Xcode tests/previews requires validation.

#### Open simulator policy decisions

- Does the allowed set name specific device instances, device types and runtime
  versions, or both? Can the configuration specify preferred defaults?
- How should an agent express why another simulator configuration is necessary?
- Is the concurrency limit shared across all Shoal runs in an environment, with
  optional lower project/run limits? Do unrelated running simulators count toward
  the budget even though Shoal must not shut them down?
- Should idle instances remain booted until capacity is needed, or expire after
  a timeout? What state must be reset before another agent receives an instance?

### Cooperation and enforcement

Resource and port reservations are cooperative: Shoal tracks allocations and
avoids assigning the same exclusive resource or port to multiple owners at once.
Agents are expected to request resources and use their assigned instances and
ports. Shoal does not currently need to prevent agents from using an unallocated
resource or binding a different port.

Reservations do not guarantee that unrelated processes will leave a port free.
How Shoal detects and handles an already occupied port remains an implementation
decision. Enforcing port use and resource access is a possible future extension.

Filesystem sandboxing remains in the product direction to limit filesystem
access. Containing agents that deliberately try to bypass restrictions is not a
current requirement.

### Proposed vocabulary

These terms are a starting point for discussion, not a finalized API.

- **Run:** one managed agent execution, with its workspace and environment.
- **Resource instance:** an individual allocatable simulator or other
  shared facility.
- **Allocation:** a record associating an owner with a resource or port until
  release or recovery.
- **Pool:** a collection of resource instances from which a caller can request
  a suitable instance.

### Open decisions, in priority order

1. **First usable workflow:** should the first version allocate resources to
   externally started agents, or already launch and track those agents itself?
2. **Coordination scope beyond v1:** v1 coordinates across projects for the
   current OS user within one execution environment. Cross-user and host/guest
   coordination remain open future decisions.
3. **Resource lifecycle policy:** should Shoal create dedicated simulator devices
   or use existing ones? Should released instances shut down
   immediately or stay running for reuse? When should their state be reset?
4. **Acquisition:** are resources requested before launch, dynamically during
   execution, or both? Can callers request any suitable instance or a specific
   named instance?
5. **Contention:** should requests wait, fail immediately, or support both with
   timeouts? How should requests needing several resources avoid deadlock?
6. **Ownership and failure:** does an allocation belong to a process, run, or
   parent orchestration session? When its owner crashes, how is it reclaimed,
   and how are surviving processes handled before reuse?
7. **Ports:** how are assigned ports delivered to applications? Must applications
   accept configurable ports? How should Shoal detect occupied ports and handle
   application bind failures under the cooperative reservation model?
8. **Integration interface:** what CLI commands, structured output, and events
   will Superlogical need? Defer project-specific assumptions until known.

## Disk usage and removal

### Confirmed requirement

Cleanup is a core design concern. Most agents should be disposable through Shoal
without leaving substantial disk usage or large numbers of leftover files.
Stopping an agent's process alone is not sufficient cleanup.

Caches and similar reusable data are an explicit exception: they may survive
agent removal for reuse by later runs. The exception does not automatically apply
to ordinary run files, logs, or artifacts. Which additional data qualifies as
reusable, and whether retained caches have pruning limits, remain open.

### Proposed approach

- Give each run an identifiable storage area for its workspace, temporary files,
  build outputs, logs, and private caches where tools support this configuration.
- Track run-owned data created outside that area, including simulator data,
  so removal can account for it.
- Stop run-owned processes before deleting their data, and release allocations
  as part of removal.
- Distinguish disposable run data, shared resources/caches, and retained results.
  Removing one agent must not delete data still needed by another agent or
  unrelated user data.
- Track retained caches separately from disposable run data. Consider retention
  limits and pruning for caches, reusable resource instances, logs, and artifacts;
  specific limits and automatic pruning have not been agreed.
- Make interrupted cleanup retryable and report remaining files or failures.
  A removal should not report complete cleanup while tracked data remains
  unintentionally.

These are proposed implementation principles, not selected mechanisms. In
unrestricted mode, cleanup still needs an ownership record and cooperation from
agents; deleting an arbitrary filesystem change is not an assumed capability.

### Open cleanup decisions

- Which agents or runs should persist rather than be disposable?
- What work must be preserved before removal: branches, commits, uncommitted
  changes, patches, or selected artifacts? How is that choice expressed?
- Are dependency and build caches shared for speed, private for simple removal,
  or configurable? What size or age limits should shared storage have?
- Should removal delete run-specific simulator devices,
  reset them for reuse, or retain them under an explicit policy?
- What logs or run metadata remain after removal, and for how long?
- What is an acceptable residual disk footprint and file count after repeated
  create/remove cycles? Numeric limits have not been chosen.

## Xcode 27 research

Reviewed 2026-09-16. Local `xcodebuild -version` reports Xcode 27.0, build
27A266a. Findings below are research inputs. The user has accepted `simctl` for
simulator lifecycle management and evaluation of `devicectl` for additional
interactions; the other proposed integration choices still need validation.

### Relevant Apple changes

The current [Xcode 27 release notes](https://developer.apple.com/documentation/xcode-release-notes/xcode-27-release-notes)
document:

- A preview MCP server that works without an open Xcode workspace, including
  persistent directory-tree permissions for code-signed agents.
- Agent tools for simulator interaction and changing active run/debug state.
- ACP (Agent Client Protocol), agent plugins, and filesystem access controls for
  agents and their subprocesses.
- Prebuilt simulator dyld caches for faster first launches, continuous
  CoreSimulator log rotation, and resource-budgeted shutdown of idle preview
  simulators.
- A remaining runtime-removal issue: some deleted runtimes reappear after reboot
  (141290052 / FB16083602).
- Parallel-test simulators may be running without appearing in Device Hub
  (176809181).
- Default Interface Builder toolchain compilation avoids requiring a downloaded
  simulator for that compilation step.

Device Hub provides a common interface for physical devices and simulators and
can run without opening Xcode. Apple also presents `devicectl` with JSON output
for automation. See [Get the most out of Device Hub](https://developer.apple.com/videos/play/wwdc2026/260/).

### CLI observations from the installed Xcode

Read-only help inspection confirmed:

- `xcrun simctl help` exposes create, clone, boot, shutdown, erase, delete, reboot,
  and runtime operations, and a `--set <path>` option.
- The same help warns that the `booted` selector chooses one device when several
  are booted. Shoal should pass the allocated device's explicit UDID.
- `xcrun devicectl --help` describes versioned JSON for automation; human-readable
  output is not a stable interface. `--json-output -` sends JSON to stdout and
  ordinary progress output to stderr, with exceptions for certain commands.
- `devicectl device --help` includes capture, process, settings, reboot, and other
  operations. This inspection does not establish it as a replacement for all
  `simctl` lifecycle commands.

No simulators were created, booted, or removed, and no Xcode MCP settings were
changed. Runtime behavior and integration compatibility have not been tested.

### Proposed implications for Shoal

#### Programmatic access to Device Hub functionality

Apple's documented automation entry point is `devicectl`, which uses the same
underlying technology as Device Hub. See the
[automation section of Apple's Device Hub session](https://developer.apple.com/videos/play/wwdc2026/260/?time=952).
No separate public Device Hub SDK or HTTP API was found in the reviewed sources;
that is a research finding, not proof that no other interface exists.

Additional installed CLI help confirms `device capture screenshot` and
`device capture screen-record`, simulated location/biometrics/status-bar controls,
and appearance/audio/VoiceOver/reset settings. Actual support must be checked for
each target and OS. The inspected help does not establish a general touch-input
API or an embeddable live Device Hub view for Macraft.

For Shoal, the accepted tool choice is `simctl` for simulator lifecycle operations
and evaluation of `devicectl` for additional device operations. Parsing structured
results is the proposed integration approach. Xcode MCP is another integration
surface for agent-facing tools; it is not evidence of a public Device Hub SDK.

#### Allocation and cleanup

- Use explicit simulator identities and structured command results. Keep the
  simulator lifecycle implementation behind an adapter so supported commands can
  vary by installed Xcode version.
- Evaluate a Shoal-owned simulator device set for storage ownership and cleanup.
  Verify that Xcode builds, tests, previews, and MCP tools can address that set
  before selecting this approach.
- Preserve Shoal's ownership of startup/shutdown when integrating agent tools:
  cooperative agents should acquire a Shoal allocation before interacting with
  the assigned simulator. Investigate whether Xcode tools can consistently target
  that allocation and whether shared Xcode run/debug state also needs a lock.
- Evaluate the workspace-independent MCP server for unattended agents in Macraft
  environments. Do not assume this also proves operation without a GUI login or
  concurrent worktree isolation; both require testing.
- Treat reusable installed runtimes and caches separately from disposable device
  data, screenshots, test results, and build outputs. Runtime retention is a
  proposed application of the cache exception, not an agreed classification.
- Verify cleanup outcomes and report leftovers. Do not interpret a successful
  removal request or disappearance from a UI as proof that all storage is gone.
- Account for simulators started indirectly by previews and tests when deciding
  concurrency limits and ownership. Shoal should not shut down unrelated devices
  merely because they appear idle.
- Check sandbox access to Apple services as well as filesystem paths during
  implementation; reading CLI help is insufficient to validate sandboxed use.

### Next validation questions

- Should v1 require Xcode 27 or support older versions through capability checks?
- Which workflow comes first: command-line build/test, Xcode MCP integration, or
  both?
- Can every chosen Xcode tool reliably use the simulator and workspace assigned
  by Shoal, including under parallel testing and preview generation?

## Broader lifecycle questions

These remain relevant to the full product but do not need to dominate the first
resource-allocation milestone.

- What signals completion: process exit, an explicit signal, or a returned result?
- Should agent process exit affect retention before the caller requests removal?
  In the intended Superlogical flow, session deletion triggers run removal.
- What happens to workers when their parent orchestrator exits?
- Which agent executables are supported first, and is session resume required?
- Do filesystem restrictions limit writes only, or reads as well? Which shared
  caches, SDKs, credentials, and other paths are accessible?
- Who may grant unrestricted execution or additional permissions to a worker?
- Do agents survive terminal closure, Shoal restart, or machine reboot?

## Filesystem isolation recommendation

These are proposed backend choices and implementation details. Seatbelt is already
the intended macOS mechanism; the Linux backend is not yet confirmed.

### Handling tool configuration and out-of-scope access

Both backends enforce the configured policy on the sandboxed agent and ordinary
child commands. A denied filesystem operation fails; the tool may report a
permissions error, use defaults, or fail to start. The sandbox does not
automatically ask the user to grant access or pause the operation for approval.
Inheritance was checked in the local macOS `sandbox(7)` manual and the
[Landlock documentation](https://docs.kernel.org/userspace-api/landlock.html).

#### Configuration-based access

Use the confirmed global/repository policy for both reads and writes. Agent and
build-tool config outside the run baseline needs a repo declaration or a global
allow. There is no default grant to read the entire home directory. Existing OS
permissions remain in force; a Shoal allow cannot grant access the OS denies.

| Location or purpose | Proposed treatment |
| --- | --- |
| Agent/build-tool config | Repo-declared or globally allowed reads; global denies win |
| Installed runtime dependencies | Explicit baseline or configured access |
| Run worktree, temporary files, logs, and build output | Proposed baseline writes, tracked for removal; global denies win |
| Approved shared caches | Declared/allowed writes, retained according to cache policy |
| Agent session state and history | Prefer run-owned paths where supported |
| Other locations | Reads and writes denied unless declared or globally allowed |

Config is not always read-only in practice: tools may save refreshed credentials,
create lock files, or store history beside their config. An agent/tool launch
profile should specify required writable paths and supported environment/path
overrides before startup. Prefer redirecting disposable state to run-owned
storage; use narrow shared write allowances where necessary. Do not grant broad
home-directory writes merely to make a tool start.

Treat the real target of a symlink as part of the access design; a link inside a
worktree does not make outside data run-owned. Include Git's shared worktree
metadata in the policy as needed for commits and other Git operations.

#### Proposed failure handling

- Show the effective path policy before launch or through an inspect command.
- Preserve the command's error and report a denied path/operation when the
  backend supplies reliable diagnostics. Do not assume every failed command is
  a sandbox denial or that every denial is observable by Shoal.
- Let the caller add a specific allowance or redirect tool state and relaunch
  with the revised policy. Do not automatically retry the command unrestricted;
  it may already have performed part of its work.
- For a simple common interface, treat each sandbox launch policy as fixed.
  Landlock restrictions cannot be relaxed in the already-restricted process;
  use a fresh launch for expanded access. Whether restarting a command versus
  the whole agent is possible depends on the eventual execution interface.

Exact agent profiles and any interactive access request workflow remain
undecided. No new approval UI is required by this
proposal; machine/caller configuration can supply allowances in advance.

### macOS: Seatbelt through a small launcher

The installed Xcode 27 SDK's `sandbox.h` marks the header deprecated and
`sandbox_init` as no longer supported. The installed `/usr/bin/sandbox-exec` is
present; its manual also marks it deprecated and documents launching a command
with a supplied profile. Apple's
[DTS explanation](https://developer.apple.com/forums/thread/661939)
distinguishes custom SBPL profiles from the supported entitlement-based App
Sandbox and states that SBPL is not documented for third-party use.

Rust can invoke `sandbox-exec` as a subprocess or wrap C interfaces. The language
choice does not change this support status.
For v1, evaluate `sandbox-exec` behind an adapter before taking on direct private
Seatbelt interfaces. Restrict the agent launch process, not Shoal's resource
manager, so the manager can continue to allocate resources and perform cleanup.
Test the chosen profiles on supported macOS releases.

### Linux: recommend Landlock for filesystem restrictions

[Landlock](https://docs.kernel.org/userspace-api/landlock.html) allows unprivileged
processes to restrict filesystem access for themselves and future children.
Its availability and supported operations depend on the enabled kernel and ABI;
file descriptors opened before restriction need separate consideration.

This matches Shoal's current scope of filesystem restrictions with cooperative
agents. Proposed implementation:

- Use a small single-threaded launcher that applies the policy before executing
  the agent. Evaluate the [Landlock Rust crate](https://landlock.io/rust-landlock/landlock/)
  for policy construction, compatibility checks, and enforcement status. Required
  restrictions must be fully enforced rather than silently downgraded.
- Grant writes to the run workspace, its temporary storage, and approved caches.
  Derive read/write access from global and repository config. Exact baseline
  runtime and execute permissions remain to be defined.
- Set `no_new_privs`, handle inherited descriptors deliberately, and check which
  filesystem operations the kernel can restrict, including truncation and rename.
- Probe support and require the capabilities needed by the configured policy.
  Report an unsupported sandbox instead of silently launching unrestricted.
  Explicit unrestricted mode remains available for caller-selected environments.
- Leave network and port enforcement outside scope. Filesystem restrictions do
  not provide a separate process namespace or solve process cleanup.

[Bubblewrap](https://github.com/containers/bubblewrap) is an alternative if Shoal
later needs a separate filesystem view or namespace isolation. It provides a
mount namespace with selectable read-only and writable mounts and optional
additional namespace isolation. Check host/container support before selecting
it. Do not add automatic fallback between different isolation models in v1.

#### Global deny rules and Linux backend selection

The global allow/deny requirement must be validated before selecting Landlock.
Its path rules grant access to hierarchies; a broad parent grant cannot simply
be overridden by a deny entry for one child in the same ruleset. See
[Landlock policy layers](https://docs.kernel.org/userspace-api/landlock.html#layers-of-file-path-access-rights).

Consequently, "allow a directory but never allow this subdirectory" requires a
careful backend design. Do not approximate it by granting the parent anyway.
Bubblewrap's mount layout or a constrained policy translation may be needed;
the implementation must either honor the effective rules or report that the
policy is unsupported. The earlier Landlock recommendation is provisional
pending this check.

### Bubblewrap versus Landlock

Comparison reviewed 2026-09-16. The Linux backend remains a recommendation,
not a confirmed selection.

| Concern | Landlock | Bubblewrap |
| --- | --- | --- |
| Mechanism | Kernel access rules in the existing filesystem | Separate mount namespace with selected mounts |
| Filesystem visibility | Restricts supported operations; does not construct a new directory tree | Can omit paths entirely or expose them read-only |
| Run storage | Shoal creates run directories and cleans them up | Can supply private temporary mounts; persistent writable mounts still need cleanup |
| Deployment | Kernel must enable Landlock and required operations | Installed `bwrap` and permitted unprivileged user namespaces |
| Rust integration | Library-driven policy and launcher | Spawn a CLI with explicit mount and namespace arguments |
| Process lifecycle | Separate Shoal responsibility | Optional PID isolation and parent-death behavior can help |

Mechanism and availability sources:
[Landlock kernel documentation](https://docs.kernel.org/userspace-api/landlock.html),
[bubblewrap overview](https://github.com/containers/bubblewrap).
Mount and lifecycle options:
[bubblewrap manual](https://github.com/containers/bubblewrap/blob/main/bwrap.xml).

#### Important differences for Shoal

- Landlock leaves some filesystem actions unrestricted, including `chmod` and
  `stat`. It is not a complete read-only filesystem boundary. Truncation control
  requires ABI 3 or later; inherited open descriptors need separate handling.
  See the [documented filesystem limitations](https://docs.kernel.org/userspace-api/landlock.html#filesystem-flags).
- Bubblewrap can present read-only mounts and a private `/tmp`. Its protection
  depends on the complete mount/policy configuration; exposing another writable
  route to the same data can defeat the intended boundary. Its current upstream
  implementation relies on user namespaces; the old setuid mode was removed.
  See the [upstream design](https://github.com/containers/bubblewrap).
- Bubblewrap's `--die-with-parent` could help stop a sandbox with its supervisor.
  Shoal must attach that lifetime to the run launcher, not the short-lived
  preparation command. Private temporary mounts disappear with their namespace;
  bind-mounted worktrees, artifacts, and caches persist. See its
  [lifecycle and mount options](https://github.com/containers/bubblewrap/blob/main/bwrap.xml).

#### Assessment and recommendation

The user has clarified that isolation does not need to be that strict. Given
the confirmed cooperative-agent scope, recommend Landlock as the first Linux
backend. Its documented limitations do not by themselves rule it out for the
desired guardrails. It preserves the existing machine layout and avoids designing
a mount layout for every SDK, package manager, and local service. This is an
implementation assessment, not a measured compatibility result or final backend
selection.

Bubblewrap's separate filesystem view, private temporary mounts, and optional
PID isolation do not justify adding them for the current requirements. Revisit
it only if those requirements change. The exact read/write allowances remain to
be defined, but a complete read-only boundary is not a selection requirement.

For either choice, prototype using a real project: restore dependencies, build,
rename and truncate files, access shared caches, and remove a run with active
subprocesses. Include Git's shared worktree metadata and machine-installed tools
in the access design. Check the actual host/container capabilities rather than
assuming the backend works in every Macraft environment. Retain the separately
selected unrestricted mode. No automatic fallback or two-backend implementation
is proposed for v1, and no Linux execution tests have been run here.

## Implementation language

**Confirmed: implement Shoal in Rust.**

Use a portable core with narrow macOS and Linux interfaces for sandbox launch
and process handling. Keep the implementation independent of Superlogical and
Macraft. Invoke installed `git` and `simctl` commands as needed, with Worktrunk
behind Shoal's workspace creation and removal interface.

Rust's [process API](https://doc.rust-lang.org/std/process/struct.Command.html)
supports arguments, environment, working directory, and I/O setup. Its
[Unix process extensions](https://doc.rust-lang.org/std/os/unix/process/trait.CommandExt.html)
include process groups.

Model run and allocation states explicitly. Keep sandbox setup out of the
long-lived resource manager; evaluate a separate launcher process before adding
complex pre-exec behavior. Rust does not automatically terminate surviving child
processes or recover allocations and files after crashes. Those require explicit
lifecycle handling and validation.

## Local service architecture

**Confirmed:** use a CLI backed by a small local daemon. Ship both in one Rust
binary, with an internal service subcommand. Provide `shoal setup` to configure
the daemon as an OS-managed service for the current user. Communication uses a
local Unix-domain socket; a separate server binary is unnecessary. Exact service
registration and automatic startup behavior remain implementation details.

Shared allocation does not inherently require a daemon. Independent CLI
processes could coordinate through a transactional database and locks. That
would cover basic port reservations and exclusive resource claims. However,
queued requests, simulator boot/shutdown transitions, idle shutdown timers,
process monitoring, and cleanup after a caller disappears need coordination
beyond the lifetime of a short CLI command. A local daemon gives these duties
one continuing owner.

Responsibilities:

- The CLI handles commands, structured output, and waiting for responses.
- The daemon owns allocation decisions, wait queues, simulator lifecycle,
  persistent run/resource records, and cleanup/recovery coordination.
- A generic execution wrapper runs in the caller's terminal, applies the run's
  environment and sandbox, and registers its execution with Shoal. Superlogical
  continues to own terminal sessions; the daemon need not proxy terminal I/O.

Start with one daemon per OS user per execution environment, shared across
projects, using a local Unix-domain socket restricted to that user. Startup
must ensure concurrent CLI invocations cannot create competing daemons.
Cross-user and host/guest resource coordination remain separate open decisions.

The eventual recovery design must persist ownership and in-progress lifecycle
operations. After a daemon restart,
reconcile records against actual processes and simulator state before making
resources available again. A CLI disconnect or daemon crash is not evidence
that an agent has stopped or that its resources can safely be reassigned.
Exact supervision and caller-loss detection mechanisms remain open.
Advanced supervision and crash recovery belong to the later polish phase;
they are not prerequisites for the initial CLI/daemon milestone.

The service should stay available while it has runs, booted managed simulators,
queued work, or cleanup to manage. Whether it exits when fully idle is an
implementation choice. Restart recovery does not imply resuming agents after
a machine reboot.

## Implementation order

**Confirmed sequence:**

1. **CLI and daemon foundation.** Establish local communication, service status,
   and basic daemon startup/shutdown. Add `shoal setup` to set up the daemon with
   the operating system. Proposed setup behavior is repeatable registration,
   starting the service, and verifying that the CLI can reach it. Platform
   service integration and behavior in minimal containers remain to be designed.
2. **Workspaces and runs.** Accept a repository path or URL, create worktrees and
   run environments, execute the configured workspace setup command, and support
   ordinary run launch and explicit removal. Establish basic ownership records
   and cleanup for the normal workflow.
3. **Ports.** Add cooperative named port reservations, environment delivery,
   and release on run removal, coordinated across projects by the daemon.
4. **Simulator access.** Add simulator allocations, allowed/preferred device
   configuration, concurrency limits, waiting, and boot/shutdown management
   through `simctl`, using installed runtimes only.
5. **Polish and lifecycle reliability.** Refine the CLI and integration behavior,
   diagnostics, and cleanup. Handle surviving child processes, interrupted
   removal, caller failures, and daemon crash recovery here. These advanced
   lifecycle features are deliberately deferred until the core workflow works.
6. **Filesystem restrictions, last.** Implement and validate Seatbelt and the
   selected Linux backend, including repository path grants and global policy.

Earlier milestones run without Shoal filesystem restrictions and must describe
that accurately. The policy sections describe the eventual behavior, not a
requirement to implement sandboxing before the resource-allocation workflow.
Basic ownership tracking belongs with each feature; comprehensive recovery
comes later. Early versions must not claim reliable crash recovery before it is
implemented and tested.

## Proposed CLI

This command surface is a discussion draft, with `shoal setup`, the `add` and
`rm` names, `claude` and `codex` shortcuts, workspace naming,
current-directory resolution for execution commands, and
interactive target pickers confirmed. Use fzf for selection.
Use short top-level commands for runs and grouped commands for daemon
administration and resource allocation.
Here a run is the managed workspace and its allocations; it can exist before
an agent starts and after an agent exits.

### Stage 1: service setup and administration

```text
shoal setup
shoal daemon status
shoal daemon start
shoal daemon stop
shoal daemon restart
```

`setup` registers the per-user OS service, starts it, and checks connectivity.
It should be safe to repeat. It is separate from a repository's workspace setup
command. Daemon stop/restart controls the service, not run removal; behavior
with active operations must be defined before implementing these commands.

### Stage 2: runs and workspaces

```text
shoal add [<repo-path-or-url>] [--ref <git-ref>] [--name <name>]
shoal list
shoal inspect [<run>]
shoal exec [<workspace-name>] -- <command> [args...]
shoal claude [<workspace-name>] [-- <args>...]
shoal codex [<workspace-name>] [-- <args>...]
shoal stop [<run>]
shoal rm [<run>]
```

- `add` prepares the worktree and environment and executes the configured
  workspace setup command before reporting readiness. It does not launch the
  agent. Accept a workspace name via `--name`; if omitted, prompt for it.
  The created workspace directory's final component is exactly that name.
  Return the name, stable run ID, and workspace path; source-ref defaults,
  branch naming, and setup-failure retention are still open. A workspace name
  does not by itself specify which Git branch to use.
- `list` summarizes runs. `inspect` returns detailed state, workspace path,
  environment information, and allocations.
- `exec` launches an arbitrary command in the run, using its working directory
  and environment, and later its filesystem policy. It inherits the caller's
  terminal streams and propagates the command's exit status. This is the generic
  integration entry point for a session manager. An explicit workspace name
  refers to the name assigned by `add`. Without a name, use the managed
  workspace containing the caller's current directory (including subdirectories).
  If the caller is outside a managed workspace, open a workspace picker.
- `stop` stops run-owned processes while retaining the workspace. Resource
  release behavior needs a defined policy; ownership must not be released
  while processes still use an allocation.
- `rm` stops remaining processes, releases allocations, and removes
  run-owned disposable data. Preservation of branches, commits, and dirty
  worktrees must be decided before automatic removal is implemented.

### Agent shortcuts

**Confirmed:** `shoal claude` and `shoal codex` are convenience commands using
the same execution path as `shoal exec`:

```text
shoal claude fix-login       = shoal exec fix-login -- claude
shoal codex fix-login        = shoal exec fix-login -- codex
shoal claude                = shoal exec -- claude
shoal codex                 = shoal exec -- codex
```

An explicit workspace name wins. Otherwise use the workspace containing the
current directory; outside a workspace, open the workspace picker. The same
non-interactive behavior, environment, terminal I/O, exit status, ownership
tracking, and eventual filesystem policy apply to both shortcuts and `exec`.

Proposed argument convention: everything after `--` is forwarded unchanged to
the agent executable. This separates an optional workspace name from agent
arguments. Agent installation and executable availability remain machine
responsibilities; these shortcuts do not introduce separate agent integrations.

### Interactive selection

**Confirmed:** use the external `fzf` executable for interactive selection, and resolve
`exec` from the current directory before opening a picker. In particular:

- `shoal add` offers recently used repositories, most recently used first.
  History includes repositories previously supplied by local path or URL and
  persists across CLI invocations. Register a path or URL first with
  `shoal repo add`; `add` then accepts its path, source URL, or ID. After repository selection,
  prompt for a workspace name unless `--name` was supplied. Collect both before
  creating the workspace.
- `shoal rm` uses the workspace containing the current directory (including
  subdirectories), falling back to the workspace picker when outside one.
  An explicit workspace name always takes precedence.
- `shoal inspect` and `shoal stop` offer existing runs. Rows should
  distinguish repository, run name/ID, branch, and state.
- `shoal exec <workspace-name> -- <command>` selects the named workspace.
- `shoal exec -- <command>` uses the workspace containing the current directory;
  outside a workspace it opens the workspace picker.
- Resource operations follow the same selection convention when an existing
  target is omitted. Their exact argument syntax remains to be settled.

Proposed interaction details:

- Bare `shoal` opens a workspace list with `fzf` in an interactive terminal.
  Enter navigates, Ctrl-D removes, Ctrl-E offers Claude/Codex/custom shell
  command execution, Ctrl-A adds, Ctrl-O inspects, and Ctrl-S stops commands.
  An add-workspace row remains available when the list is empty. Each action
  returns to the shell. `shoal --help` shows help; bare noninteractive calls also
  show help. `shoal cd [workspace]` provides explicit navigation through the same
  shell integration (or prints the path if the integration is not loaded).
- Explicit targets bypass selection, so integrations can call the same commands.
- Non-interactive invocations and `--json` never prompt. `add` requires a
  repository and name; `exec` can still resolve the current workspace without
  prompting. If required input cannot be resolved, report missing arguments
  rather than waiting for terminal input.
- Canceling a picker performs no action. An empty repository history explains
  how to add the first repository using a path or URL.
- Removal deletes clean branches with the same contents as main or upstream.
  Dirty/differing work opens an Abort / keep branch / delete branch picker.
  Running processes do not block manual removal; they still block automatic cleanup.

`fzf` is required for interactive pickers. There is no built-in fallback; explicit
arguments remain usable without it.

Proposed naming rules: names are unique within the daemon's workspace registry
and must be valid single directory components. Reject invalid names and name
collisions rather than silently changing the requested directory name. Resolve
the current workspace using registered workspace paths, not just a matching
directory basename. The parent directory used to store workspaces remains open.

### Stage 3: ports

```text
shoal port reserve <run> <name>
shoal port list [<run>]
shoal port release <run> <name>
```

Repository-declared ports are reserved during creation, before the workspace
setup command, and supplied in the run environment. Explicit commands support
additional reservations during execution. Repeating a reservation for the same
run and name returns the existing assignment. A later reservation cannot alter
the environment of an already running process; its caller must use the returned
port. Releasing a reservation does not stop a server using it.

### Stage 4: simulators

```text
shoal sim list
shoal sim acquire <run> [--device <device-type>] [--runtime <runtime>]
shoal sim release <allocation-id>
```

`acquire` selects an allowed simulator, boots it if necessary, and returns an
allocation ID and explicit device UDID once ready. Omitted selection flags use
configured preferences. Wait/no-wait and timeout options remain to be designed.
`release` ends exclusive use; Shoal's idle policy decides when the device shuts
down. Normal agent workflows need only acquisition and release; lifecycle
operations belong to the allocator. Run removal releases its allocations.

### CLI output and integration conventions

- Management commands support `--json` for structured output, with diagnostics
  on stderr. Exact response schemas remain open.
- `exec` preserves the child's stdout/stderr rather than wrapping them in JSON.
- Workspace names are the primary human-facing identifiers; structured output
  also includes stable run/allocation IDs for integrations.
- Child executions receive a `SHOAL_RUN_ID` environment variable. For `exec`,
  an explicit name takes precedence over current-directory resolution, followed
  by an interactive picker if neither identifies a workspace.
- CLI calls do not depend on Superlogical-specific commands or session IDs.

## Worktrunk evaluation

Reviewed 2026-09-16. The agreed direction is to put
[Worktrunk](https://github.com/max-sixty/worktrunk) behind Shoal commands for
workspace creation and cleanup. The adapter still requires validation; no
installation or execution test was performed for this evaluation.

### Useful overlap

- Worktree creation, branch/path resolution, interactive selection, and JSON
  creation results are documented in [`wt switch`](https://worktrunk.dev/switch/).
- Configurable worktree paths can support Shoal's directory layout, but Shoal
  must retain its workspace-name registry independently of branch names.
  [Configuration](https://worktrunk.dev/config/).
- Hooks support workspace setup. Shoal must choose one owner for setup execution
  so it does not run twice, and later ensure setup receives the filesystem policy.
  [Hooks](https://worktrunk.dev/hook/).
- Removal supports foreground execution, JSON results, and preserving branches.
  The default background removal and branch-deletion behavior should not
  implicitly determine Shoal's cleanup policy.
  [Removal](https://worktrunk.dev/remove/).
- Its experimental `wt step tether` connects process-group lifetime to worktree
  removal, making it useful research for the later lifecycle phase. This does
  not establish Shoal's full ownership and crash-recovery guarantees.
  [Tether](https://worktrunk.dev/step/#wt-step-tether).

### Integration assessment

The Rust [library API explicitly declares itself unstable](https://github.com/max-sixty/worktrunk/blob/main/src/lib.rs).
Recommendation: evaluate a narrow CLI adapter during the workspace phase before
considering a direct library dependency. Keep the first CLI/daemon milestone
independent of the adapter implementation.

An adapter would map Shoal workspace names to explicit paths/branches, request
structured results, control hook execution, and wait for removal to finish before
reporting cleanup complete. Validate custom naming, dirty-worktree behavior,
partial failures, and concurrent operations before shipping the adapter.
Repository URL acquisition, recent-repository history, run identity, environment
delivery, allocations, and later restrictions remain Shoal responsibilities.

Worktrunk's `hash_port` maps strings into a fixed port range. This is not a
reservation mechanism: collisions and occupied ports remain possible, so Shoal
still needs its allocator. [Filter documentation](https://worktrunk.dev/hook/#worktrunk-filters).

Worktrunk embeds the Rust `skim` library for its picker. Shoal uses the external
`fzf` executable, as explicitly selected by the user.
[Picker dependencies](https://github.com/max-sixty/worktrunk/blob/main/Cargo.toml).

### Repository setup and Shoal-owned workspace commands

**Confirmed interface:** `shoal add` creates workspaces and `shoal rm` cleans
them up, invoking Worktrunk underneath. Shoal retains overall lifecycle
ownership, workspace names, directory naming, and picker behavior. Users do
not need to invoke `wt` directly for this workflow.

Separate repository registration remains the proposed prerequisite. The exact
registration syntax and whether first use may register a repository remain open.
Illustrative workflow:

```sh
shoal repo add /path/to/repo   # Proposed repository registration command
shoal add /path/to/repo --name fix-login
shoal claude fix-login
shoal rm fix-login
```

Repository registration is distinct from `shoal setup`, which installs the
per-user daemon service. It records repository identity and configuration.
Registration of a URL needs a policy for obtaining and retaining the local
repository. Existing local repositories need not be moved.

Shoal coordinates Worktrunk operations with workspace records, environment
preparation, port reservations, and dependency setup. Setup must run once with
the assigned environment before Shoal reports readiness. Removal stops owned
processes, releases allocations, and invokes worktree removal; retire the
workspace record after successful cleanup. Failed removal remains visible.
Direct external removal needs reconciliation in the later reliability phase.

The adapter must preserve the workspace name as the directory's final component
without requiring it to equal the Git branch name. Hook behavior and config
overrides must support this ownership model. `shoal exec` and its shortcuts
remain the execution entry points for environment and eventual sandbox setup.

## Proposed Rust code structure

This is an implementation outline, not a confirmed dependency selection or an
instruction to implement all phases at once. Start with one Cargo package and
one `shoal` binary. Separate concerns with modules; split into crates only if
the code later warrants it.

Suggested eventual layout (add modules when their phase begins):

```text
src/
  main.rs                 # Dispatch CLI mode or internal daemon mode
  cli.rs                  # Parse commands; normalize agent shortcuts
  client.rs               # Connect to daemon; send requests; receive results
  protocol.rs             # Versioned request/response types and framing
  model.rs                # Repository, workspace, execution, allocation types
  config.rs               # Read and validate machine/repository configuration
  paths.rs                # Service socket, state, cache, and workspace locations
  ui.rs                   # Naming prompts, picker, human/JSON output
  daemon/
    mod.rs                # Socket server and shutdown
    coordinator.rs        # State transitions and allocation decisions
  store.rs                # Durable records and schema migrations
  workspace.rs            # Add/remove workflow and setup readiness
  execution.rs            # Launch in caller terminal; report process state
  resources/
    ports.rs              # Named port allocation
    simulators.rs         # Device selection, queue, boot/shutdown, idle policy
  adapters/
    worktrunk.rs          # wt subprocess calls and structured results
    service.rs            # OS user-service setup/start/stop/status
    simctl.rs             # Xcode simulator subprocess calls, macOS only
  sandbox/                # Added in the final phase
```

### Boundaries

- The CLI prompts and renders output. It resolves omitted workspace targets
  using the caller's directory and daemon records. The daemon never opens a
  picker or uses its own current directory to infer the caller's workspace.
- `claude` and `codex` normalize to the same execution request as `exec`.
- The daemon owns persistent state and decides whether operations are allowed.
  Clients use its protocol rather than reading/writing its database directly.
- OS service administration runs on the CLI side, so `setup`, `start`, and
  service diagnostics work even when the daemon is unavailable.
- Worktrunk and simctl adapters contain subprocess mechanics and output parsing;
  workspace/resource modules determine policy. Pass argument arrays to tools,
  rather than constructing shell command strings.
- The execution wrapper stays in the caller's terminal and launches the child
  using a daemon-provided execution plan. Terminal streams do not travel through
  the control socket. Later sandbox setup belongs at this launch boundary.

### Data model

Use distinct typed identifiers for repositories, workspaces, executions, and
allocations. In code, distinguish the persistent workspace from a particular
command execution; the earlier design vocabulary used "run" for both together.

```rust
struct Workspace {
    id: WorkspaceId,
    repository: RepositoryId,
    name: String,
    path: PathBuf,
    state: WorkspaceState,
}

struct Execution {
    id: ExecutionId,
    workspace: WorkspaceId,
    state: ExecutionState,
}

struct Allocation {
    id: AllocationId,
    workspace: WorkspaceId,
    resource: ResourceKey,
}
```

These are illustrative fields, not complete schemas. Allocations are proposed
to belong to workspaces so successive commands can use the same ports and
simulators. An execution ending does not imply workspace removal. Exact release
policy and mapping to the existing `SHOAL_RUN_ID` convention remain to be settled.

### State and slow operations

Use explicit workspace states such as `Preparing`, `Ready`, `Removing`, and
`Failed`. A single coordinator serializes decisions and state transitions,
while slow external operations run as background jobs and report completion.
Do not hold a global lock or database transaction while Worktrunk, setup, or a
simulator boot runs. Conflicting operations on one workspace are rejected or
queued; unrelated workspace operations can proceed.

For example, `add` reserves the name and records `Preparing`, then creates the
worktree, assigns its environment/resources, and runs setup. It becomes `Ready`
only after required work succeeds. Failures retain enough ownership information
for inspection and cleanup. This basic state model does not implement the later
comprehensive crash-recovery phase by itself.

### Protocol and dependencies

Recommend a small versioned request/response protocol over the Unix socket,
using newline-delimited JSON with bounded frame sizes, request IDs, and
structured error codes. Check protocol compatibility on connection. Keep
control messages separate from child stdout/stderr and external-tool logs.
Mutations must not be retried blindly after a lost response; request
deduplication/reconciliation belongs in the reliability design.

Candidate libraries:

- [clap](https://docs.rs/clap/latest/clap/) for command parsing.
- [Tokio](https://tokio.rs/tokio/tutorial) for the daemon's asynchronous I/O,
  jobs, and timers.
- [Serde](https://serde.rs/) with JSON serialization for protocol messages.
- [rusqlite](https://docs.rs/rusqlite/latest/rusqlite/) for SQLite persistence
  when workspace records are introduced. Keep blocking database work off the
  asynchronous I/O workers and transactions short.

Use concrete modules initially. A plugin framework or universal resource trait
is unnecessary for ports and simulators with different lifecycle rules.

### First implementation slice

Implement only command parsing, service registration, a foreground internal
daemon entry point, socket connection, protocol/version checking, and status.
Then connect the public service start/stop/restart commands. Validate setup
repeatability, concurrent startup protection, daemon availability reporting,
and CLI-to-daemon communication before adding workspace operations. SQLite,
Worktrunk, resource schedulers, and sandbox backends are later slices.

## T3 Code integration research

Reviewed 2026-09-16. This is a future integration candidate, not a replacement
for the Superlogical workflow or an addition to the initial implementation scope.
No T3 Code instance or integration was installed or tested.

### Current capabilities

T3 Code documents remote control through T3 Connect, direct/private-network
pairing, and desktop-managed SSH connections. The execution server and agent
work remain on the target machine. It also offers optional balancing of new
threads across eligible connected machines; existing threads remain where they
started. [Remote access](https://github.com/pingdotgg/t3code/blob/main/docs/user/remote-access.md).

T3's server owns provider processes, terminals, Git, and project files; clients
control it through authenticated RPC.
[Architecture](https://github.com/pingdotgg/t3code/blob/main/docs/internals/overview.md).
It can create worktrees for threads and use existing ones.
[Threads](https://github.com/pingdotgg/t3code/blob/main/docs/user/thread-sidebar.md).

### Proposed integration boundary

Run the T3 server and Shoal daemon on the same execution host/environment, with
an integration on the T3 side calling Shoal's generic CLI. T3 supplies its remote
connection and conversation UI; Shoal stays a local workspace/resource service.
This does not require exposing Shoal's Unix socket over a network or making
Shoal implement T3's server protocol.

Two levels are worth evaluating:

1. **Workspace/resource coexistence:** create a workspace through Shoal, use that
   existing checkout in T3, and let cooperative agents call Shoal for resources.
   T3-launched processes do not automatically receive Shoal's environment,
   lifecycle tracking, or future sandbox merely by using the same directory.
2. **Full lifecycle integration:** a T3-side adapter calls Shoal for workspace
   preparation, routes provider launches through its generic execution boundary,
   and requests explicit cleanup when the workspace is no longer needed.
   Prevent duplicate worktree creation and coordinate all threads sharing one
   workspace before removal. Client disconnect or thread settlement alone must
   not be treated as workspace deletion.

The second level is proposed engineering work. No supported, ready-made Shoal
adapter or complete lifecycle extension point was verified in this review.

T3 uses provider-specific programmatic interfaces; for example, its Claude
adapter wraps Agent SDK sessions. Launching `shoal claude` in a terminal alone
does not establish a T3 conversation integration.
[Claude adapter](https://github.com/pingdotgg/t3code/blob/main/apps/server/src/provider/Layers/ClaudeAdapter.ts).
Provider binary-path settings may offer a wrapper integration route, but its
behavior must be tested rather than assumed.
[Provider settings](https://github.com/pingdotgg/t3code/blob/main/docs/user/providers-claude.md).

Design implication: `shoal exec` must preserve piped stdin/stdout/stderr as well
as terminal streams, avoid interactive prompts for machine callers, and support
explicit workspace identity. Protocol output from a provider must not be mixed
with Shoal status messages. Full launch integration must preserve the provider's
arguments, working directory, signals, and exit behavior. Keep T3-specific
session and provider logic outside Shoal's core.

## Distribution and Homebrew

**Confirmed future requirement:** Shoal should be installable through Homebrew.
This is packaging work and does not change the feature implementation order.

Proposed approach: publish a formula, initially through a project-maintained
tap. Installation supplies the `shoal` binary; `shoal setup` remains the public
entry point for configuring the daemon. The tap name and supported release
targets remain open. See the [Homebrew formula cookbook](https://docs.brew.sh/Formula-Cookbook).

Design implications:

- Keep writable configuration, state, workspaces, and caches outside the package
  installation directory so upgrades do not replace them.
- Service registration must reference an executable location that remains valid
  across upgrades and must work with different installation prefixes.
- Account for an older running daemon after a CLI upgrade through protocol
  compatibility checks and a defined restart/migration path.
- If Homebrew service integration is offered, it and `shoal setup` must manage
  one service identity instead of registering competing daemons. The exact
  service-manager integration is still to be designed.
- Declare required runtime tools in packaging once adapter requirements are
  settled. Shoal continues not to install repository build tools itself.

No formula or release automation has been implemented yet.

## Implementation status

Rust is the selected implementation language. The local CLI/daemon architecture
is confirmed, with one OS-managed daemon per OS user per execution environment
and a `shoal setup` command. The implementation order above is confirmed. The
exact recovery protocol, remaining resource CLI syntax, and Linux sandbox
mechanism remain undecided. SQLite is used for workspace persistence.
The simulator lifecycle tool choice is `simctl`, with `devicectl` to be evaluated
for additional interactions.

The first implementation milestone is complete: a single Rust binary, service
setup and controls, a versioned JSON protocol over a private Unix socket,
foreground daemon mode, singleton locking, and stale-socket recovery. Runtime
state defaults to `~/.local/state/shoal`, with an explicit override for isolated
development instances. macOS uses launchd; Linux uses systemd user services.
Service-manager tests use an isolated fixture; native Linux service operation
has not yet been validated. The foundation is committed before workspace work on
`feat/workspaces`.

### Initial workspace milestone

Implemented:

- Repository registration from local paths or Git clone URLs. URL clones are
  retained for reuse; no automatic fetch. Repository pickers show recent use first.
  Repository lists display the name and original path or URL. Pickers show only
  names, adding `(hostname)` when names collide, or `(local)` for local sources;
  if that still collides, append the source. Internal IDs still identify selections
  and remain in JSON output; UUID cache paths are never used as picker labels.
  Registration is idempotent by remote URL (or canonical path when no origin
  exists). Local checkouts use `origin`; common HTTPS/SSH forms and optional
  `.git` suffixes match. Host, repository path case, and explicit port differences
  remain distinct. Existing registrations are checked on each add, without network
  access; old duplicate records are not automatically deleted or merged.
- SQLite repository, workspace, and execution records in `state.db`. The daemon
  owns all writes. Name reservations are atomic across concurrent requests.
- `add`, `list`, `inspect`, `exec`, `stop`, `rm`, and `claude`/`codex` shortcuts.
  Worktrunk creates/removes worktrees with hooks disabled and its own isolated
  configuration. Commands after `--` are passed as argument arrays.
- Workspace paths `<state-dir>/workspaces/<name>`; unique names are 1–64 ASCII
  letters/digits/hyphens/underscores and start with a letter or digit. A new
  `shoal/<name>-<unique-suffix>` branch starts from committed `HEAD` or `--ref`.
- Manual removal compares Git trees with local `main` and the branch's configured
  upstream. A clean worktree matching either is removed together with its branch,
  regardless of differing commit history. Missing refs do not match. Otherwise
  show three fzf choices: Abort (default), delete worktree but keep branch, or
  delete worktree and branch. Both deletion choices discard uncommitted files;
  a retained branch preserves committed work only. Noninteractive callers use
  `--yes --keep-branch` or `--yes --delete-branch` for these cases.
  Manual deletion stops connected Shoal commands, but external/disconnected
  processes do not prevent removal and are not killed. Automatic cleanup still
  blocks on any running/unknown execution or process using the directory.
  Worktrunk reports the actual branch result (including another-worktree guards).
- External `fzf` repository/workspace pickers, with no built-in fallback.
  Explicit targets and noninteractive/JSON operation never open a picker.
- Connected execution wrappers preserve terminal or piped I/O and exit codes,
  receive stop requests from the daemon, and terminate the command's process
  group. Removal first stops connected executions. The environment includes
  `SHOAL_WORKSPACE_ID`, `SHOAL_RUN_ID`, `SHOAL_WORKSPACE`, and `SHOAL_STATE_DIR`.
- `shoal shell init` prints Bash/Zsh integration; successful `setup` prints
  `source <(shoal shell init)` to add to shell configuration. The function enters
  a newly created workspace and moves out of a removed
  current workspace to the registered repository root, falling back to home.
  Navigation uses a temporary file containing a literal directory path, without
  evaluating that path as shell code. JSON calls never request navigation.
  Keep shell logic minimal: Rust chooses the destination; the function only
  passes the result back to the shell, changes directory, and preserves status.
  Shoal does not modify personal shell configuration.

Repository configuration parsing and automatic setup/dependency restoration
remain unimplemented while the public format/schema is undecided. Ports,
simulators, and sandboxing remain later milestones. This slice handles normal
connected execution, not comprehensive crash recovery: unknown executions block
automatic cleanup, and detached processes need later supervision work. Manual
removal may proceed without stopping disconnected processes. No reconciliation
command exists yet.

Validation uses real Worktrunk with temporary repositories/state, including
concurrent name claims, dirty-removal refusal, execution I/O and exit codes,
stopping commands, persistence, and Bash/Zsh navigation. Real `fzf` repository/workspace selection and foreground
terminal input are checked through a temporary pseudo-terminal.


### Automatic worktree cleanup

Confirmed and implemented: enabled by default, with a 10-minute idle timer.
Only clean worktrees whose HEAD commits are reachable from locally known remote
branches and which have no running commands/processes are eligible. No automatic
fetch is performed. Unknown executions and failed inspections block cleanup.

The daemon polls approximately every 30 seconds. Filesystem metadata changes
(including ignored files), Git HEAD changes, and Shoal command activity reset the
timer. Dirty/unpushed/busy states cancel it; becoming eligible starts a new timer.
Timers reset after daemon restart. Metadata scans do not follow symlinks outside
the worktree. Open shells count as active use for automatic cleanup. Process
checks use `lsof` and fail closed when unavailable. Changes are rechecked in the
shared removal path immediately before deleting the worktree; cooperative
processes are assumed, so this is not transactional isolation against arbitrary
external filesystem changes.

Global `~/.config/shoal/config.toml` accepts:

```toml
[auto_cleanup]
enabled = true
idle_minutes = 10
```

`enabled = false` disables automatic cleanup. The delay must be 1–525600 minutes.
An absolute `XDG_CONFIG_HOME` overrides the config directory. Restart the daemon
to apply changes. Unknown configuration fields are rejected to catch typos.
Repository configuration and the later machine/filesystem policy remain separate
future work.

**Resource ownership is per worktree.** Both manual and automatic removal use
one lifecycle: check/confirm, stop managed commands, release/reset worktree-owned
resource leases, remove the worktree, then retire its ownership record. Future
port/simulator backends must implement release in this shared path and retain
ownership records on failure. Those backends are not implemented yet; no actual
resource release is claimed by this milestone.

### Named port reservations

Implemented cooperative TCP reservations owned by worktree ID:

- `shoal port reserve <name> [workspace] [--port N] [--env VARIABLE] [--reason TEXT]`
- `shoal port list [workspace]` or `shoal port list --all`
- `shoal port release <name> [workspace]`

Missing workspace arguments resolve from the current directory, then fzf.
Names are required and stable within a worktree; a repeated reservation returns
its existing port and can update its reason. Names contain lowercase letters,
digits, `_` or `-`, starting with a letter (max 64). Optional reasons are nonempty
single lines (max 256 bytes). Changing a number or environment mapping requires
release first. Explicit release is cooperative and does not stop processes.

SQLite stores reservations with uniqueness for port numbers across the daemon,
names within a worktree, and environment mappings within a worktree. Allocation
uses a short write transaction and probes IPv4/IPv6 TCP sockets without address
reuse. Probe sockets are closed immediately; unrelated processes can still bind
later. UDP and socket activation/enforcement are outside this milestone.

Default automatic range: 49152–65535. Global TOML `[ports]` supports `start` and
`end`; explicit `--port` can select another nonzero port that can be bound.
Subsequent executions receive `SHOAL_PORT_<NORMALIZED_NAME>` or an explicit
`--env` variable. Existing process environments are not changed. Reservations
survive command exit, stop, and daemon restart. `inspect` and JSON output expose
the name, owner, port, environment variable, and reason.

Successful manual and automatic removal release port records in the same
transaction that retires the worktree. Failed directory removal retains leases.
Live resources such as simulators will additionally need shutdown/reset before
directory deletion. Resource activity resets the automatic cleanup timer.

### Repository names and worktree diffs

`shoal repo add <path-or-url> --name <name>` assigns a custom name; repeating an
existing repository updates the name instead of adding it again. `shoal repo
rename <repository> <name>` changes it later. Custom names are unique, use the
same syntax as workspace names, appear in fzf, and serve as repository selectors.
Default names still come from the repository source/origin, with hostname
qualifiers when picker names collide.

`shoal diff [workspace]` runs native `git diff` from the appropriate fork point.
The recorded base branch, rather than its current tip or only a frozen original
commit, is used with `git merge-base --fork-point` and a regular merge-base
fallback. This excludes changes imported from main when main advances or the
worktree rebases/merges it. The original base commit and full ref are recorded
when creating worktrees. Explicit fixed-commit bases use their captured commit;
older workspaces without metadata use main. Missing/unrelated base refs produce
an error. Staged and unstaged tracked changes are included; untracked files are
not part of Git diff. Configured Git pagers and external diff commands are honored.
The interactive workspace list also offers Ctrl-F for diff.

Database migrations preserve existing ownership records while adding base
metadata, repository names, and port reservations. Protocol version changes
require restarting older daemons with the installed executable.

### Repository port defaults and workspace command scope

Repository configuration is now TOML, at `.shoal.toml` or `.shoal/config.toml`.
Both together are an error. The latter allows scripts alongside the config.
Only port settings are implemented in this slice; other proposed keys remain
future work. Config is read from the current worktree at request time.

```toml
[ports]
on_conflict = "suggest" # default; "auto" accepts another available port

[ports.web]
port = 3000
env = "PORT"
reason = "Frontend dev server"
# on_conflict = "auto" # optional per-name override
```

Workspace creation never allocates ports. `shoal port reserve web` uses this
configuration; `--port`, `--env`, `--reason`, and `--on-conflict` override it.
Existing reservations remain stable, including when auto allocation selected a
number different from the configured preference. Explicit changes to an existing
number/environment mapping require release first.

Suggestions make no reservation. Interactive callers accept through fzf;
noninteractive/JSON callers receive the requested and suggested numbers and exit
code 2 (`reserved: false`). Accept by requesting `--port <suggested_port>`;
a race produces another suggestion. Successful output always includes the actual
port. `shoal ports [workspace]` shows configured and reserved ports together,
defaulting to the current worktree. `port list` shows only actual reservations.

Processes launched through `exec`, `claude`, or `codex` inherit a random
`SHOAL_SCOPE_TOKEN`, checked by the daemon on every request. Their list/repository
views are filtered; they can inspect/diff/execute in their own worktree and
manage its resources. They cannot create/remove/stop worktrees, alter repositories,
control the daemon, or access other worktrees. Nested executions inherit the
restriction. Tokens expire when an execution disconnects/finishes or the daemon
restarts. Local CLI service administration is also denied when scoped.

The human or orchestrator that manages multiple worktrees must call Shoal outside
a scoped execution. This is the agreed cooperative model: a same-user process
could deliberately discard its environment or use Git/filesystem tools directly.
Filesystem restrictions remain a later implementation milestone.

### Implemented simulator sharing

This section supersedes the open scheduling/configuration choices above.
Simulator operations use `xcrun simctl`, verified against the installed CLI help
and Apple's [Xcode command-line tool reference](https://developer.apple.com/documentation/xcode/xcode-command-line-tool-reference).
`devicectl` and Device Hub integration are not required for this slice.

Global `~/.config/shoal/config.toml` (restart the daemon to apply):

```toml
[simulators]
max_booted = 2
max_devices = 4
idle_seconds = 120
allow_any = false
default = "phone"

[simulators.profiles.phone]
device = "iPhone 17"
runtime = "iOS 26.5"
```

Profiles are the allowed device-type/runtime combinations, using exact names or
identifiers from `shoal sim catalog`. Only installed, available runtimes qualify;
when the inventory provides runtime compatibility data, incompatible device types
are rejected before creation. Ambiguous names fail; identifiers are recommended.
No default profile is assumed on a machine without configured profiles. Limits
require 1–64 running slots and max_devices between max_booted and 256; the idle
grace is 0–86400 seconds. These limits apply to this daemon; multiple isolated
Shoal state directories do not share a scheduler.

Repository TOML can select ordered preferences:

```toml
[simulators]
preferred = ["phone", "tablet"]
```

Shoal picks the first installed compatible preferred profile; explicit CLI flags
override preferences. Repo config cannot expand machine policy. `allow_any = true`
permits other installed combinations through `--device` with `--runtime`; those
requests require `--reason`. Shoal never downloads runtimes or installs Xcode.

```sh
shoal sim catalog
shoal sim acquire [workspace] --profile phone --name tests
shoal sim acquire [workspace] --profile phone --wait 60
shoal sim list [workspace]
shoal sim list --all
shoal sim release tests [workspace]
```

The default lease name is `default`. Acquisition is explicit and exclusive, owned
by worktree, with idempotency for the same worktree/name. Different names can
request multiple instances. Output includes a concrete UDID, runtime, device
type, instance ID, lease name, owner, reason, and state. Use the UDID with
`xcodebuild -destination 'platform=iOS Simulator,id=<UDID>'` or `simctl <command>
<UDID>`, never the ambiguous `booted` selector. `inspect` includes simulator records.
Scoped processes can acquire/release/list only their own worktree's devices;
`--all` is filtered. Catalog exposes installed types/runtimes and policy, not
other worktrees' leases. Commands without a target stay in the execution's scope
even if a child changes its current directory.

Scheduling and storage:

- A single simulator transition lock serializes allocation, boot, release,
  shutdown, reset, and deletion. SQLite transactions do not span subprocesses;
  unrelated CLI/port requests stay responsive. Boot readiness is checked with
  `simctl bootstatus <UDID> -b` and a structured inventory check, with a 180-second
  timeout per simctl invocation.
- Prefer compatible idle instances and preserve apps/settings on normal handoff,
  including across worktrees. Only an explicit `--clean` request authorizes an
  erase; see the clean-device policy below.
- At the running limit, shut down least recently used unallocated Shoal devices.
  Active leases are never preempted. External devices in simctl's default device
  set count against capacity but Shoal never mutates them. Other device sets and
  indirect preview/test clones are outside this initial inventory boundary.
- At the storage limit, delete least recently used unallocated devices before
  creating another. Released devices expire after the grace period; a 15-second
  sweep shuts them down and deletes their data. This bounds stored device count;
  installed runtimes and OS caches remain machine-owned.
- Capacity exhaustion returns a structured busy result and exit code 2. `--wait`
  retries for up to the specified number of seconds (max 3600); it waits for
  capacity, with boot time separately bounded. No FIFO/fairness guarantee or
  atomic multi-resource acquisition is implemented yet.
- Simulators use the default CoreSimulator device set for compatibility with
  Xcode destinations. Only devices associated with persisted Shoal records are
  mutated. Creation saves a unique `shoal-<instance-id>` name before calling
  simctl; interrupted creation can recover the UDID by that exact name.
- Ownership survives daemon restarts, command exit, and failed operations. A
  failed/interrupted allocation is retained and requires `sim release` or worktree
  removal before reuse. A restarted daemon never infers that a lease became idle
  merely because its execution connection disappeared. Simulator list state is
  Shoal's recorded lifecycle; external changes are checked on allocation/cleanup.
- Active simulator leases prevent automatic worktree removal. Manual removal
  stops connected commands, shuts down/deletes assigned and last-used idle
  devices, then removes the worktree. Failed simulator deletion retains its
  record and worktree so cleanup can be retried. Already successfully deleted
  resources stay deleted if a later worktree-removal step fails.

Validation: isolated fake-simctl integration tests cover exclusivity, concurrent
claims, waiting, state-preserving handoff, explicit resets, external-device protection, machine limits,
allowed/any policy, missing runtimes, crash persistence, interrupted creation,
failed-removal retries, idle expiration, and worktree cleanup. A native disposable
smoke test on this Mac also created/booted an iPhone 17 with installed iOS 26.5,
verified repeated acquisition, released it, and confirmed deletion after removal.
No persistent test service was installed. Simulator execution remains macOS-only;
Linux retains CLI/workspace/port functionality.


### Explicit clean-device acquisition and accountability

Normal acquisition preserves simulator apps, data, permissions, and settings.
Ownership transfer alone never causes an erase. Cooperative agents must finish
using the app/debugger before release. `sim acquire --clean --reason <text>`
requests a newly created or fully erased device. The reason is required both by
CLI argument validation and by the daemon, and must be a nonempty single line
(max 256 bytes). This is a per-request choice, not an automatic machine policy.
Existing active leases cannot be reset in place; release first or use another
name. A repeated ordinary acquire remains idempotent; a new clean request for an
already-active name fails without wiping it.

Selection minimizes reinstalls: use spare pool capacity to create a fresh device
without losing any installed apps. At capacity, compare estimated user-app counts
on idle devices. Prefer erasing a compatible low-cost instance, or replace an
incompatible idle instance when that loses fewer apps. Never preempt active
leases. Cached counts come from `simctl listapps` (parsed via macOS `plutil`),
which needs a booted device. Refresh counts on release and inspect booted idle
candidates for clean selection; do not boot stopped candidates just to count.
Unknown counts rank last, and ties use least recent use. System apps are excluded.
These counts estimate reinstall cost, not elapsed time or app size. An empty app
list is not proof of pristine device settings, so a reused clean allocation is
always erased. Shoal does not automatically reinstall erased apps.

Before acting, persist a clean-request audit record in the state SQLite database.
Record workspace ID/name, repository ID, daemon-verified execution ID when scoped,
request UUID, lease/profile/device/runtime selection, reason, timestamps, attempt
count, status, action, device IDs, estimated app loss, planned evictions, erase
completion, and failures. Missing/invalid reasons submitted directly to the daemon
are recorded as failures; CLI parse errors never reach the daemon. Unscoped
callers are labelled as such. Repeated capacity polls retain one request UUID
and update its attempt count/status; terminal request IDs cannot reset devices
again. Persist failures/busy results, and mark unfinished requests interrupted
when the daemon restarts. If an audit write fails, do not start a destructive
operation. An interrupted operation's log does not claim it completed.

`shoal sim history [workspace]` resolves the current worktree; `--all` reviews all
worktrees, including removed ones. `--limit` (default 20, maximum 50) and `--before
<audit-id>` paginate newest-first, with JSON support. Scoped callers remain
limited to their own worktree. Audit rows have no cascading workspace/device
foreign key and are retained after removal; there is no automatic audit pruning
in this slice. History supports reviewing unnecessary requests; reason quality
is not inferred automatically. Direct same-user simctl/database operations remain
outside the cooperative restriction model.

Validation adds normal-handoff preservation, fresh-capacity preference, low-app
reset/eviction selection, daemon-side reason enforcement, no reset of active
leases, failed-erase logging, scoped visibility, retry grouping, and retained
history after removal/restart. A disposable native iOS simulator also verified
real application-list parsing and audit retention after cleanup.


### Agent shortcut launch arguments

`shoal claude [workspace] -- <args>` appends `--remote-control <workspace-name>`
to the forwarded arguments. Resolve the workspace first so an ID, current-directory
match, or fzf selection yields its human name for the remote-control session.
`shoal codex [workspace] -- <args>` appends `--sandbox workspace-write
--ask-for-approval=never`. These defaults apply only to the shortcuts; generic
`shoal exec` passes its command through unchanged. Shoal's worktree scope still
applies to all executions. Stub-executable tests verify exact argv and name
resolution without starting actual agents or remote-control sessions.


### Implemented generic resource pools

Generic resources use cooperative counting semaphores. They are declarative
names and capacities, with no lifecycle adapter, process enforcement, proxy, or
sandboxing. A standalone resource is a one-member pool. Capacity 1 is a mutex.

Both global and repo TOML support `[resources.<name>]` with `capacity` (default
1) and optional `reason`, and `[resource_pools.<name>]` with an optional total
`capacity`, optional `reason`, and named `[resource_pools.<name>.resources.<member>]`
entries with their own capacity/reason. Omitted pool capacity is the sum of member
capacities; empty pools are invalid. Names use the same lowercase format as
ports, and capacities must be 1–65535. Standalone and pool names cannot collide.

Global definitions are shared across all registered repositories in one daemon.
Repo definitions are shared by worktrees of the same registered repository and
are independent of equally named pools in other repos. Different repo definitions
cannot override a global name; an identical duplicate resolves to global scope.
Members are identified within their pool, not by a cross-pool global name.
Definitions are loaded like other settings: global at daemon startup, repo from
the selected worktree at request time. No allocation occurs at creation.

`shoal resource acquire <pool-or-resource> [workspace]` acquires one permit from
both a pool and one named member. `--resource` pins a member; otherwise select
an available member with the lowest fraction of capacity occupied, breaking ties
by name. `--name` identifies the lease (default `default`) for idempotent repeated
requests. Distinct names request additional permits. Explicitly changing the
member of an existing lease requires release first. `--reason` overrides member
or pool defaults and can update an existing lease's reason. Output identifies
the actual member, pool, scope, owner, lease ID/name, reason, and acquisition time.

`shoal resources [workspace]` shows configured pool/member capacity, aggregate
occupancy, availability, config disagreements, and the caller's leases.
`resource list [workspace]` lists actual leases; humans may pass `--all`.
`resource release <pool-or-resource> [workspace] [--name <lease>]` returns a permit
without requiring the config definition to still exist. Target selection follows
ports/current directory/fzf, and scoped requests are authorized by the daemon.
Scoped agents see aggregate shared occupancy but never other owners' lease
records, and cannot release or inspect another owner's leases.

Allocation uses an immediate SQLite transaction covering readiness checks,
existing-name lookup, definition agreement, both capacity checks, and insertion.
Concurrent acquisitions cannot overbook. Definitions are recorded with leases;
while any are active, a conflicting branch/global definition rejects new claims.
Existing matching leases remain usable and releasable. Once drained, a new
request can replace the stored definition. This prevents branch config drift
from creating extra capacity or revoking already-issued permits.

Busy acquisition returns `acquired: false`, code `resource_busy`, and exit 2.
`--wait <seconds>` retries up to 3600 seconds; no FIFO fairness, multi-permit
atomicity, or deadlock avoidance is implied. A permit survives command exit and
daemon restart until explicitly released or its worktree is removed. Active
permits block automatic cleanup. Manual removal stops connected commands, removes
the directory, then releases permits with the ownership record in one transaction;
failed removal retains them. Agents must stop external resource use before
release, since Shoal cannot verify use of generic resources.

Validation covers independent pool/member limits, mutexes, multiple named permits,
concurrent claims, bounded waiting, idempotency, persistence, scope filtering,
repo/global identity, definition drift, release after config removal, successful
and failed removal, and automatic-cleanup protection. The root agent skill adds
only the resource commands and acquisition/release semantics.

### Typed workspace and execution states

`WorkspaceState` has Preparing, Ready, Stopping, Removing, and Failed variants.
`ExecutionState` has Running and Unknown variants (Unknown is a recorded loss of
execution connectivity, not an arbitrary unknown input). Models, comparisons,
transitions, SQLite bindings, and JSON use these types. Database and wire values
remain the existing lowercase strings, preserving saved state and CLI consumers.
Unknown/mismatched values fail decoding rather than silently creating a new state.
Round-trip compatibility tests cover both JSON and SQLite representations.
