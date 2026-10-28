# Reviewing Shoal

Rust CLI plus a local daemon that owns shared state for coding-agent
workspaces, ports, simulator leases and permits. `AGENTS.md` holds the working
rules and `docs/design.md` the product decisions; `docs/reference.md` documents
behavior. When a change touches lifecycle, ownership, resource leases or
removal, check it against the rule those files state before judging it.
Repository code audits use [audit.md](audit.md) for inspection instructions
instead of the PR delivery guidance below.

## What matters most

- Managed daemon updates wait for tracked execution records and daemon operations
  to finish; write replies only while holding an operation. Preserve the socket
  backlog and ownership lock across exec; read-only streams reconnect after the
  handoff, preserving event cursors, and continue in the installed release when
  the protocol changes. Only requests a daemon refused unrun are sent again.
- The minimum Rust version follows current stable; keep the manifest and CI
  toolchain aligned and validate with locked dependencies.
- Treat function abstraction as a blocking correctness check. Require every
  function to stay at one abstraction level: orchestration calls named domain
  operations while helpers own lower-level implementation details. Flag mixed
  levels before weighing style or test coverage, even when behavior is correct;
  judge responsibility boundaries rather than function length or helper count.
- Raise these module guidelines as advisory findings, not blocking ones:
  - Give each module one responsibility a reader can name. When a file comes to
    cover several, or a match dispatches every command of a group, split it by
    responsibility into a directory of modules before adding to it; judge the
    responsibility, not the line count.
  - Tests live in a sibling `tests.rs` (`#[cfg(test)] mod tests;`), not inline in
    the implementation file.
  - Integrations such as agent tools, terminal hosts and apps implement callbacks
    for what they do differently; state, decisions and daemon calls stay in
    Shoal's generic code, which passes each callback only what it reads.
- Safety of user work: removal goes through the one shared path, preserves
  work and leases on failure, never adopts moved or replaced worktrees, stops
  every verified process carrying the workspace identity, and never signals a
  process whose recorded identity was not verified. Herdr
  completion waits for tracked executions to resolve or for Herdr to report the
  agent's turn finished, and for stopped work to be resumed, before closing the
  tab; removal closes it immediately. Unreadable
  inventory entries block ownership proof only while their recorded identity is live;
  recovery polls incomplete proof within a bounded wait without weakening it,
  and workspace stopping covers child registration, stopping and completion verification;
  service termination and its command deadlines cover the drain.
  Worktree ownership is proven by the owner marker in the Git admin directory; the recorded
  inode/birth-time identity proves only unmarked records, which gain a marker only
  after it still matches; missing proofs never establish ownership.
  Only an explicit reclaim (`work adopt` of the recorded path) may
  re-establish ownership that failed verification, and only at the recorded path,
  repository and branch. Automatic recovery only readies a workspace the startup
  audit failed once its ownership verifies again; it never accepts another failure,
  such as a failed setup. Any new
  path that deletes a directory, kills a process or mutates a simulator
  without those checks is a blocker. Repository deletion confirms the resolved
  registration, source and checkout path, preserving that target across renames.
  Repository IDs and sources win before path lookup. Names survive unrelated
  caller-path collisions; a name and path
  matching different registrations are ambiguous and list both candidates.
  Automatic removal of a contribution repository waits the cleanup grace period,
  then rechecks, under the gates workspace creation takes and in the transaction
  that records the removal, that no workspace, queued agent, chat, pending issue
  proposal, kept branch, borrowed resource, unexpired checkpoint or removal
  record, or checkout work the remotes' advertised branches and tags lack remains;
  removing one with any of them is a blocker.
  Workspace removal confirmations list uncommitted changes
  and untracked files with Git status codes, bounding the preview and reporting omitted
  entries. Environment checks and the untracked-worktree listing only report,
  as alerts and inbox items; dependency checks use the shared executable catalog.
- `[env]` layers per variable from saved repository config, worktree, global,
  then default. Tracked executions, hooks and workspace exports substitute known
  workspace and reserved-port fields once without shell or recursive expansion;
  `SHOAL_*` names stay reserved and port exports win collisions. Desktop handoffs
  may reuse existing processes and do not receive configured variables.
- Untracked hooks use global defaults below repository config.
  Manual workspace maintenance lives under `work`; setup runs automatically on
  creation and `work setup` reruns it explicitly.
  Pre-setup hooks run untracked in the daemon after ownership and execution
  checks, with a timeout; failure gates readiness even without a setup command.
  Lifecycle and permit changes are excluded while the hook runs. Removal rechecks
  ownership and policy after hooks before selecting branch deletion; idle cleanup
  requires unchanged HEAD and activity, and manual removal refuses unsaved work its
  confirmation did not cover.
  Post-remove runs from the repository checkout after ownership is released; failure is a
  warning and notification, never restored ownership. Already-missing worktrees
  skip hooks, and post-remove events are not replayed on restart. Post-done runs
  after completion persists and before cleanup;
  failure notifies without undoing completion or blocking cleanup. It excludes
  lifecycle and permit changes and is not replayed on restart. Agent-exit hooks
  expose tracked agent exits while ready, including disconnects, without changing
  the exit result or marking completion; removal uses its own hooks.
- Every problem names the command that resolves it; repairing a workspace
  remains an explicit choice except readying one the startup audit failed once
  its ownership verifies.
- Scope: tracked executions and externally launched processes with exported
  environments carry a scope token and get access to their own worktree and the
  child workspaces split from it, recursively, only (CLI refusals also follow ancestry
  for the same state directory); without a target a scoped command acts on the child
  whose directory it runs in, while agent hooks always report for the own workspace.
  Workspace allocation other than `split`, removal, recovery and shared repository/service
  administration stay denied. For the workspaces below their own, scoped callers queue agents,
  which the daemon starts detached so they never inherit the caller's scope, send agent messages,
  answer questions and discard stopped work; the nearest ancestor with an agent receives a child agent's messages,
  questions, completion and ready marks and its agent's exits and stops instead of the user,
  except what a process causes in a child without an agent of its own. Own-workspace setup, rename, base workspace, PR registration,
  merge acknowledgements, messages to the user, reading their own agent messages, ready-for-review marks,
  assignment completion and its withdrawal, listing, opening and changing issues and PRs
  of the own repository except merging, rerunning or cancelling their CI jobs,
  starting, rerunning or cancelling CI runs of their own branch and reading the own repository's,
  GET requests to the own repository's forge API, or others where `[api] agent_writes`
  allows them, own-repository sync and
  effective-configuration reads are allowed;
  only unscoped callers export or revoke environment tokens. Export requires a
  ready, verified worktree; tokens persist across restart until revocation or
  workspace removal, without execution tracking or cleanup protection.
  Notifications are read by the unscoped user only and never fail the
  operation they record; an agent message is the operation, so its failure is
  returned, and it never records completion. Only unscoped callers, and the
  coordinating agents of the workspaces below theirs, queue messages for a workspace's agents, and delivery deletes a message. Agent hooks deliver
  messages and queued item activity, replacing a running watch, and a Claude Code
  hook wakes the idle agent; a chat's hooks deliver its own queue of its workers'
  events, which the user receives as notifications if the chat ends first; a workspace's own Shoal comments, thread replies,
  closing and reopening are not reported back to it. The only terminal input Shoal writes is
  the tracked wrapper's fixed prompt into an idle Codex's zmx session, once per
  idle stretch; the daemon never writes to a terminal. Workspace events are an unscoped, durable lifecycle
  stream with replay gaps, independent of notification read state. Ready-for-review
  marks bind to HEAD, cover only linked items or the unlinked workspace, record
  journal events, and never notify, complete, or change cleanup; `post_ready_cmd`
  failure keeps them. Only explicit
  done completes an assignment. PR cleanup
  defaults on and removes a completed workspace only after every watched PR merges
  and the set contains HEAD or the default branch has its changes; disabling it pauses only that, never linking; completion honors the done default and `--cleanup` before
  stopping tracked agents with clean files and unchanged merged HEAD. PR
  numbers resolve against the workspace's origin and persist as repository-bound URLs.
  Own-workspace PR waits poll activity independently of cleanup and share persistent
  cursors, returning separate completed CI contexts as well as discussion, conflict
  and PR state changes. Lookup failures must not masquerade as successful checks
  or mergeability; agents handle updates and wait again. Repeated reads are
  conditional on the validators of the account's last response, and a 304 only
  stands for a body remembered for that account and URL. A forge's rate limit
  holds back that account's requests until it ends and slows polls with a
  notification instead of producing lookup failures. Pending deliveries remain
  replayable until CLI output succeeds and is acknowledged.
  Registration variants preserve existing stored and JSON shapes and reject ambiguous
  records and conflicting actions without blocking other workspaces' cleanup;
  manual acknowledgements bind to exactly the recorded HEAD. Completion cleanup
  requires preserved commits without a PR registration, honors existing merge
  checks, and rechecks its recorded HEAD; a completion without cleanup blocks idle
  and PR cleanup. Holds, open questions, a pending PR proposal, child workspaces and queued agent
  launches block every unattended removal, rechecked after the removal reserves the workspace; a split needs a ready
  parent, checked when the child is recorded; `done` closes the workspace's open
  questions for good. A scoped land
  is an approval request bound to the branch HEAD unless `[land] requires_approval`
  is false; approval never skips a landing check and lands only the approved commit.
  Under `[approvals] issue_open`/`pr_open = "propose"`, a scoped `issue open` or
  `pr open` records a proposal instead of opening; approval opens it as the agent
  account through the normal open path, a PR only at its recorded commit, and an
  issue proposal belongs to the repository and outlives its workspace.
  `[issue] other_repositories` governs a scoped issue in another repository the same
  way (`"propose"` by default) or refuses it with `"deny"`; it is never linked. This is cooperative,
  not a security boundary, so judge it as such.
- Simulator plans never authorize mutations by themselves: the executor rechecks
  recorded ownership and live capacity under the simulator gate. Indexed workspace
  lookups use current ownership, falling back to last ownership only when unclaimed;
  malformed ownership must not disappear from filtered results.
  Omitted runtimes select the latest installed, available compatible iOS version;
  explicit runtimes stay pinned and existing leases keep their runtime.
- Queued requests claim new capacity in arrival order per pool, and for
  simulators; while any remain, every other new claim is busy. Renewals and
  approval requests ignore the queue, and a request in progress keeps its place.
- Allocation is atomic and persisted before the external mutation (simctl,
  Worktrunk). Leases survive restart and failed removal and are released only
  with successful removal. Active permits block automatic cleanup. Caller-named workspace holds persist
  across restarts, block automatic removal without deferring completion, and are
  released with successful removal; deleted-worktree cleanup still forgets them.
  Holds belong to the user and host apps: scoped callers list their own
  workspace's holds but cannot acquire or release them. Permit hooks
  run after persistent acquisition and before release, including removal while
  the worktree exists; failure retains leases. Retrying acquisition reruns its
  hook. Permit/lifecycle changes cannot overlap resource hooks in one workspace;
  user scripts own external resource integrations.
- Numbered questions and approvals inspect records without mutation in JSON or
  nonterminal output; interactive prompts preserve the shown request when deciding.
- Resource approvals bind effective allocation settings and require an unscoped
  decision, or one by the coordinating agent a child workspace's request was routed
  to because it holds an approved grant for the same target and settings; it may pass
  the request up instead. That decision, which queues an agent message for the requester; failing to queue it
  never fails the decision. Pending requests reserve nothing; grant lifetime follows configuration
  and approval never bypasses capacity or simulator cleanup rules. Approval lookups
  validate their selected ID or active workspace/target/name record; grant reuse
  validates every workspace/target candidate, including released grants. Listings
  reject invalid selected records; malformed history outside a query does not block it.
- Explicit creation bases resolve locally and are recorded for diff; only a base
  naming the local default branch is refreshed, and one naming a remote branch is
  fetched first. A base naming another workspace's branch records that workspace
  as the base workspace; base changes stay in the same repository without cycles,
  and removal moves stacked workspaces down to the removed workspace's base.
- Once every watched PR of a base merges and covers its HEAD, the PR sweep restacks its stacked
  workspaces before cleanup can remove it, retargets only their PRs still
  targeting its branch, and queues a watch update; it never rebases or pushes.
  Forge writes are retargeting, the daemon's typed issue and PR actions and
  `shoal forge api` requests, sent through the REST transport; scoped callers cannot
  merge. `shoal forge api` paths stay under the forge's API root. Forgejo and Gitea API
  requests use fj's saved token for the repository's own host, validated, passed
  to curl on stdin, never logged, stored or sent elsewhere, or else tea's login
  through `tea api`; GitLab requests go through `glab api`. Bitbucket requests
  send the token `[bitbucket] token_command` prints, validated, only to
  api.bitbucket.org on curl's stdin, never stored or logged. The forge is chosen
  by a pinned `[forges."<host>"] type`, a well-known host name, or else the
  host's own answer to each forge's version or metadata endpoint, asked with
  only the logins saved for that host and never gh's or glab's environment
  tokens, which the daemon records so the host is asked once and asks again
  when it starts or reloads its config; a pasted link of a host not yet resolved is classified by its pin,
  else by the forge its routes name.
  A request needing a capability the forge's backend does not declare is
  refused with the typed `Unsupported` error before anything is sent; per-forge
  gaps belong in the backend's declaration, not in separate checks.
  Views automatically include failed Actions logs, read by the daemon as the
  workspace's agent account, resolving web indices to API IDs and validating the PR head before returning
  a bounded tail; scoped callers cannot access another workspace's logs.
- `[secrets]` values are fetched in the CLI by user commands run without
  input, at agent launch or, for tool-scoped secrets, by the tool's launcher on
  each run; the daemon, state and logs never hold them, hooks drop them and
  their launchers, executions other than agent launches fetch none of their
  own, and worktree files cannot set them. A gated tool secret is fetched only
  after the daemon confirms the workspace's grant; only tool secrets can be gated.
- Daemon ref updates disable Git credential and SSH askpass prompts without
  overriding the user's SSH transport; interactive Git retains normal prompting.
  Production Git command arguments live in `git`; callers use named operations
  and retain lifecycle and execution policy.
  Interactive prompts and pickers live in `cli::ui`; flag a command that runs
  fzf or reads the terminal itself, or adds a picker or prompt `ui` already offers.
  Ancestry and exact-ref failures stop branch selection, refresh, and removal;
  only a documented negative exit status is a negative answer.
- Explicit workspace paths override only that creation; reject overlaps with state,
  checkouts, workspaces, and other repository directories; never delete their parents.
  Resolve existing symlinks and lexical `..` before checking and creating repository
  roots or explicit workspace paths, including when trailing components are missing.
- A repository directory moves only whole into a new root while nothing uses it:
  reserve its workspaces and record the move first, switch records only once the
  directory arrived, and let recovery finish a completed move or roll back an
  incomplete one; never delete the source before the records name the destination,
  nor any directory whose identity differs from the recorded one.
- Worktrunk compatibility is shared across creation and existing-branch selection:
  reserved names get a leaf suffix on creation and an error on opening; suffixes
  never change derived workspace names or directories. Adapter outcomes preserve
  unfamiliar strings without treating them as confirmed branch deletion.
- Workspace rename preserves the recorded path and ownership while changing the
  checked-out branch and derived name. Existing PR watches block rename because
  their remote head remains the original branch. Reserve intent before changing Git;
  no execution other than the scoped caller may be recorded.
  explicit repair reconciles interrupted intent without replaying the mutation.
- `pr open` pushes to the branch's push remote and opens the PR in `origin`'s
  repository; a push remote on the same forge host is a fork the PR comes from,
  and contribution clones keep `origin` as the repository itself.
- Pasted links select repositories by origin identity: `add` opens issue, PR or
  branch links; `link`/`unlink` manage issue and PR completion associations. `watch`
  defaults to linked items, supports kind filters and explicit items without
  changing completion. Cursors are workspace/item-owned, recheck associations
  before recording linked activity, and unlinking clears only that item's cursor.
- Existing branches, including an issue's derived local branch, use unsuffixed
  worktrees; reopen verified owned workspaces, require explicit adoption for other
  linked checkouts, never adopt the main checkout,
  and on removal delete a branch only when a remote or the default branch has all its
  commits, never the default branch. Adoption records
  identity and readiness atomically without setup, hooks, or Git setting changes;
  it takes normal cleanup ownership and cannot repair moved managed worktrees.
  Idle cleanup accepts commits on remote-tracking branches and commits merged into
  the local default branch or its remote-tracking branch, including squash and
  rebase merges.
- Releases are pinned to the merged version commit; changelogs use published
  ancestor releases and merged PRs excluding release preparation; deleted branch
  labels require matching repository and exact PR identity. GitHub
  keeps change descriptions with a GitHub comparison link, omitting unmirrored
  Forgejo PR and issue references. Runtime dependency links target the tagged
  README on each host; reruns synchronize notes without replacing
  complete asset sets. Homebrew releases install checksummed binaries after
  both release hosts publish assets; only `--HEAD` builds from source with Rust.
- Prompt templates append the selected saved/worktree repository value to global
  guidance, falling back to the bundled chat template for an omitted global base.
  Empty repository values suppress only local additions; provenance reports the
  combined text and highest configured layer. Inline TOML wins over Markdown.
  Shoal's built-in agent guidance precedes every worker template, which holds
  only additions; the former `agent_template` name still loads with a warning. `install` preserves existing template files; substitutions never evaluate or
  recursively expand inserted text.
- Issue numbers use an explicit repository, the current registered checkout or
  managed workspace, or an interactive picker. Issue URLs must match the selected
  repository's remote; unregistered issue or PR URLs offer interactive callers to
  register the repository URL and otherwise name the registration command. Lookup failures or already-closed issues create nothing. The
  canonical URL persists before tracked setup or agent launch and cannot be replaced.
  Reopening follows the issue association before its derived local branch name.
  Withdrawing completion preserves associations and merge checks but never defers
  cleanup; only holds, open questions, child workspaces and queued agent launches keep a workspace.
  Associated issues suppress idle cleanup, not merged cleanup.
  Tracked agent exits wake cleanup after the exit hook, retaining its usual checks.
  Merged cleanup ignores links, watches, completion and saved stopped work,
  dropping queued restores and keeping agents in removal's record for restore,
  but never holds, open questions, running/unknown commands, leases, directory users or a Git
  operation under way; its idle period is persisted so restarts cannot shorten
  or reset it, and removal rechecks its snapshot through the shared path.
  Every unattended removal except for disk space waits `[cleanup] grace_minutes`,
  started only once removal's initial checks pass and ended by any sweep pass that
  no longer removes the workspace; user requests never wait.
  Every removal of an existing worktree saves a checkpoint and removal record
  before deleting files and keeps both when removal fails; merged cleanup may
  discard unsaved files only once the checkpoint keeps them, and restoring a
  checkpoint first keeps the state it replaces in another.
  Lookup failures and changed origin identity never count as closure.
- Single-workspace actions use an explicit target, then scope or current directory,
  then an interactive picker; a deleted current directory provides no workspace
  context. Explicit misses never fall back. Noninteractive and
  JSON calls without a current workspace need a target. Bare `cd` always picks;
  all-workspace operations and checkout-aware `config show` keep their selection.
- Shell integration recovers deleted current directories and pending destinations
  to their nearest surviving ancestor without a daemon, preserving exit status and
  existing prompt hooks. Scoped callers cannot use this recovery.
- Herdr handoffs belong to the CLI after interactive choices, preserving resolved
  choices, literal arguments and state/config selection; tab labels use
  `<repo>#<number>` for issues and the allocated workspace branch otherwise by
  default. Optional `herdr.tab_name` templates follow config precedence, substitute
  fields once and travel with the plan; branch fields update after allocation.
  Preparation failures keep the tab readable; agent exits keep the tab open,
  and completion or workspace removal closes it.
  Default focus follows the resolved launch: agents started with an issue prompt
  or forwarded arguments stay in the background, tabs waiting for input receive
  focus, and explicit focus settings override either default. `--focus` also
  starts the agent past `max_agents`, as `--now` does; the `focus` setting does not.
  JSON, help, noninteractive and `--here` calls run in place. Shell and untracked
  desktop handoffs retain the tab.
- Commands refuse invalid given arguments before any prompt for omitted ones;
  interactive `add` has the daemon check its creation request first.
- Agent pickers offer “No agent”: issue creation continues without a launch,
  and review returns without starting a reviewer. An agent whose executables are
  not on PATH is refused before any work and left out of pickers, the workspace
  menu and completion.
- Commands never name an agent or terminal host. What configuration can say about
  an agent lives in its `[ai.<name>]` description; tool-specific code lives in an
  `Integration` keyed by agent name under `cli/agents/`, and terminal-specific code
  in a `Host` under `cli/terminal/`. Optional integrations stay behind their Cargo
  feature and setting.
- Claude and Codex launches (including Happy and Codex app handoffs) persist
  trust for the repository's shared Shoal workspaces directory, or an explicitly
  placed workspace outside it, even when the user config is absent, preserving
  other settings; successful repository removal removes those trust entries while
  preserving other project settings.
- `max_agents` admission is the daemon's: it defaults to `dynamic`, which starts
  one agent at a time, staggered, while measured CPU plus the expected load of
  recently started agents stays below the warning thresholds; critical or
  failed memory and disk readings start none, and an idle machine starts one.
  It accepts a positive fixed value or `none`; the global value limits the
  machine while a repository's fixed value limits that repository. Slots derive from tracked
  agents, executions awaiting reattachment, resume records and starting queue
  entries, excluding done workspaces, never from a separate counter. A starting
  entry keeps its slot only while its launch holds it or within the grace
  period, until its agent registers; queued entries create no worktree.
  A launch with dependencies queues without a limit and starts only once each
  is done at its current HEAD, removed or closed, on the branch of the done one
  that contains the others; `--now` never waits.
- Machine-wide memory overload protection is opt-out; sustained CPU protection is
  opt-in. The machine-only `load` level sets the CPU and memory thresholds the
  file leaves out; explicit values win. Stop one connected tracked
  agent at a time through its wrapper, the one idle longest first and otherwise
  the newest, with configurable thresholds
  and timing; retain work and leases, notify with the pressure reason and recovery
  path, distinguish automatic recovery from manual or unavailable recovery, and never select disconnected
  executions. Failed readings reset the sustained timer and authorize no stop.
  Below each enabled protection's stop threshold, `[overload.warning]` queues an
  agent message for every workspace with a running agent, at most once per signal
  and workspace per repeat interval; warnings never stop, clean up or notify.
  Automatic recovery uses built-in Codex and Claude session restore commands or a
configured command for another agent, plus proven child
  termination, verified ready workspace ownership and sustained healthy headroom;
  an agent stopped between turns restores on its next update instead, with free
  disk space, holding no slot meanwhile once its processes are proven stopped.
  Unknown turns count as mid-turn.
  The protection stop, and an agent's reported exit, also stop the agent's
  identity-verified processes outside its process group; a wrapper whose stop
  cannot be proven keeps waiting.
  Keep waiting wrappers tracked and reattachable across daemon restarts and crashes,
  serialize restores and honor manual stop/removal.
  Persist overload recovery and its reason before delivering the stop, reporting save failures
  without disabling protection. Records keep the stop's stage; an unknown
  execution with a saved record is forgotten only once its wrapper exited and no
  owned process survives, rechecking survivors until they exit, and only
  survivors or incomplete evidence notify. Manual and automatic restores prompt agents to continue unfinished
  work, through `{prompt}` for configured commands, without replaying the original prompt; consume the
  selected record only after its replacement process is registered. Unrelated
  executions do not block manual recovery.
  Critical disk space stops every agent the same way, restoring once the cleanup
  threshold is free, and other executions as manual stop does; recovery requires
  that free space too.
  Manual stop, refused reattachment and graceful shutdown of wrappers that cannot
  reattach save agent recovery and user command arguments for explicit resume,
  preserve work and leases, and never rerun a recorded command. Every daemon stop
  carries its cause, which the wrapper prints after the agent stops. A hangup or
  terminate signal to an agent's wrapper or agent saves the agent's recovery the
  same way, with the signal as its reason, and is not an exit on its own.
  Scoped callers cannot stop executions.
- Command wrappers keep running when the daemon goes away and reattach only after
  the daemon verifies the record, scope token, wrapper and child identities and
  process group; the reattach request stays accepted at every protocol version.
  Detached wrappers never write to the terminal while their command runs.
- Agent forge wrappers are opt-in, resolve per tool through configuration layers,
  and change only the PATH of a tracked agent, inherited by descendants, and of
  the `gh`, `glab` and `tea` requests Shoal makes for a workspace. Wrappers own
  authentication; Shoal reads only fj's saved token, per request, never storing or logging it,
  and never switches the user's login. Forge changes for a workspace use the
  agent account except merging, which uses the user's. The agent Git profile
  reaches only a tracked agent's environment and the push of `shoal pr open`;
  it never writes Git config.
- Git profiles apply only to newly created worktrees, before setup, using
  per-worktree config; other worktrees keep their settings.
- Root help groups built-in commands by task; configured commands are discovered
  through `run`.
- Named commands follow repository/global precedence per name and use the
  tracked execution wrapper with workspace scope. CLI agent argument defaults
  are replaceable; substitutions expand once and forwarded arguments stay literal.
  Repository-only commands require a current or explicit workspace; unknown names
  report command errors with built-in suggestions without opening a picker.
  `{diff_base}` resolves lazily through the shared daemon diff-base lookup.
  AI tools (built-in providers and global `[ai.<name>]` entries) are agents:
  an `[ai]` `command` joins the global command layer, and `shoal <name>` and
  `run` launch it as `shoal claude` does; pickers and completion offer AI tools
  only. Custom agent names select named commands and run as tracked agents. Their
  `{prompt}` combines general instructions and issue context, prepended to
  forwarded arguments when no prompt placeholder is configured.
  Plain command invocations expand `{prompt}` to an empty string.
  Failed-turn retries default on for Claude and its profiles, honor the nearest
  `retry` setting, and wait out usage limits without an attempt cap; transient
  API failures keep a bounded retry budget.
- AI skill directories are machine-only configuration. Skill installation accepts
  configured tool names, needs no daemon, and remains denied to scoped processes.
  Installing for every tool writes only into skill directories that exist.
- External tools are invoked with argument arrays, never shell strings. Captured
  subprocesses share optional deadlines and bounded diagnostics, terminate on
  cancellation, and drain output while sending input. Internal CLI workers use
  one typed builder with explicit state directory and output mode.
  The execution wrapper retains buffered daemon controls across execution transitions.
  Terminal I/O stays in the execution wrapper, which restores the caller's foreground
  group and OS terminal settings after commands and interactive hooks. When `TERM` is
  nonempty and not `dumb`, it also resets emulator input modes for the shell, writing
  best-effort cleanup to the terminal rather than redirected output; stop requests
  give the entire command process group a shared grace period, even after its leader exits.
  Terminal agent launches put the tracked wrapper inside a zmx session, never a
  terminal emulator between agent and terminal; launches without a terminal, inside
  zmx, or in an enabled Herdr pane run in place. The daemon owns
  state and prepares each execution kind before shared registration through one
  request. Reported exits clear executions and permit setup readiness only after
  marker, child and group survivors are gone and environment visibility is complete;
  a settled empty environment is readable evidence, not an unreadable process; the
  reporting wrapper may remain alive awaiting acknowledgement.
  Setup verification failures retain the workspace and report its exit status
  and blocking PIDs or inspection error instead of offering to ignore uncertainty.
  CLI styles, enum `Display` formatting and transient progress belong at presentation
  sites; machine output and stored values stay plain. Progress clears before results and stays off for JSON,
  redirected stderr and dumb terminals.
- Client response extraction reports expected and received variants and preserves
  typed daemon error codes and messages without changing the wire format; unfamiliar
  codes survive verbatim for version compatibility.
  Resource protocol methods use domain-then-verb names matching the CLI operations.
  Repo resources bind a registered checkout ID and path for cooperative read access,
  including in approvals; external leases block repository deletion. Workspace
  cleanup and release never modify the borrowed checkout.
  Independent per-workspace overview reads use bounded concurrency, preserve
  workspace order and account for failures without blocking other reads.
- Persistence: SQLite work stays on its dedicated thread, with one connection and
  a bounded queue. Shutdown drains admitted operations before releasing ownership;
  cancellation or failure never replays a closure that may have committed.
  Closed Shoal enums with matching display and wire names share
  explicit spellings through one macro and reject unknown values. Opening persistence
  only migrates schema; daemon startup
  quarantines interrupted operations atomically after migration and before ownership
  auditing, requests, or cleanup. Failure aborts startup without undoing migration.
  The separate native simulator state enum preserves
  unfamiliar strings and native spellings; only confirmed shutdown frees running
  capacity. Schema and state-file changes need a compatibility
  story for existing daemons. `install` preserves compatible daemons and commands,
  deferring service changes until restart; incompatible daemons restart.
  The shell initialization hint is omitted when a startup file already initializes Shoal.
  Config key edits preserve unrelated settings and comments and validate one command's
  changes together before saving;
  repository edits serialize read/modify/write in the daemon and leave worktree files alone.
  An operation that combines global definitions with resolved settings reads both from
  one global config snapshot; reloads are serialized, replace it only with a valid
  file, and never stop agents.
  Packaged config installation replaces global TOML only on explicit request,
  keeps a backup, and uses templates embedded in the binary.
  Packaged skills use the runtime `SHOAL_SKILLS_DIR`, then the build-time
  directory, then an adjacent `shoal-skills` symlink. Explicit paths must be
  absolute; relative link targets resolve lexically against the link's
  directory. The source must hold every bundled skill and stays uncanonicalized
  so stable prefixes follow upgrades. Shoal changes or removes only skills that
  still match its per-directory record, plus unrecorded files from earlier
  versions, never other files in skill directories. Unscoped commands other than
  the daemon refresh only directories that still hold a Shoal skill. Code that
  changes user-level files outside the state directory checks
  `build::changes_user_files`, which debug builds fail unless
  `SHOAL_DEV_USER_FILES=1`.
- Tests run in temporary state directories and repositories without the launching
  environment's Shoal variables or configuration locations, use the isolated
  xcrun fixture, and never install services or touch personal simulators.
  Check timing against AGENTS.md's validation rule: tests must not depend on
  runner speed, and a wait must observe the awaited event itself, not a proxy
  that can hold first.
- Documentation: usage lives in README.md, behavior in docs/reference.md,
  decisions in docs/design.md, contributor rules in AGENTS.md. Flag behavior
  changes without the matching sentence edit, text that restates code or
  history, and sentences that list or count what a rule covers instead of
  stating the rule; an overstated rule is narrowed, not annotated with its
  exceptions. Never ask for a comment on clear code, a reworded correct one,
  or a longer document.

## Delivery

When commit history is available, check that each commit has one coherent
purpose and keeps its tests and documentation together. Flag avoidable bundles
of independent changes and suggest concrete boundaries. The size guidance in
AGENTS.md is a prompt to inspect, not a line-count gate; tightly coupled
changes are a valid reason for a larger commit. Flag a PR that changes what
Shoal prints or draws in the terminal without examples of the new interface in
its description or comments. After fixing substantial findings, an agent
requests another review and marks the PR ready only once it comes back clean.
Agents cannot merge PRs;
leave merging to the user instead of asking for permission to merge.

## Do not bother with

- Formatting and import order (rustfmt and clippy run in CI).
- Restating what a commit does, or style preferences not backed by `AGENTS.md`.

Label conventions: `area/` allows several affected areas; `type/` and
`complexity/` each allow one. Reviewer request labels are combinable:
`default` uses `REVIEW_AGENT` or Codex; reviewer requests are consumed
at run start, while `none` persists to suppress review.
Opening a ready PR already requests its review; do not add request labels in its
opening classification batch.
