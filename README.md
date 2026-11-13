# Shoal

Shoal gives each coding task its own Git worktree and coordinates shared ports,
Xcode simulators, and other resources. Run agents or commands in a workspace,
switch between tasks, and clean up when finished.
## Install

```sh
brew install hn05/tap/shoal
shoal daemon install
shoal skill install
```

Or download a prebuilt macOS or Linux binary from a [release](https://github.com/HN05/shoal/releases).
On macOS the archive holds the `shoal` binary and the same in `Shoal.app`, both
signed with Shoal's own certificate but not notarized: clear their quarantine
with `xattr -dr com.apple.quarantine shoal Shoal.app`, then put `shoal`, or a
link to `Shoal.app/Contents/MacOS/shoal`, on your `PATH`. macOS keeps the bare
binary's privacy grants only while it stays at the same path.
Homebrew installs the additional tools for workspace management and interactive
menus, but not zmx 1.0.1 or later, the default terminal host: install it with
`brew install neurosnap/tap/zmx`, or use [another terminal host](docs/reference.md#agents)
instead. Upgrading zmx from 0.8 leaves the agents already running in its
sessions out of Shoal's reach: [stop them first](docs/reference.md#agents). For a downloaded binary, install
the [runtime dependencies](#runtime-dependencies) before running `shoal daemon install`.
`shoal daemon install` starts the per-user
daemon and writes a missing config with its defaults in `~/.config/shoal/`;
`shoal skill install` installs the `shoal` skill into each
[AI tool's existing skill directory](docs/reference.md#agent-skill-outside-project-repositories);
`shoal skill install <tool>` creates one. Later `shoal` commands keep installed
skills current after upgrades.

On macOS, when workspaces or `root_dir` are on an external volume, allow Shoal
access to removable volumes when macOS asks, or grant Shoal Full Disk Access or
Removable Volumes under System Settings > Privacy & Security, then run
`shoal daemon restart`. The grant survives upgrades and covers the agents Shoal
starts through its terminal hosts or in the background, with the commands they
run.

For directory navigation and tab completion, add this to your `.zshrc` or
`.bashrc`, then run it in your current shell:

```sh
source <(shoal shell init)
```

### Runtime dependencies

Put these tools on `PATH` before running `shoal daemon install`, which captures it for
the daemon. Prebuilt binaries do not require Rust.

| Tool | Needed for |
| --- | --- |
| Git 2.43+ (`git`) | Repository and branch operations |
| [zmx](https://github.com/neurosnap/zmx) 1.0.1+, unless `terminal.default_host` names another host | Agent sessions that keep running after you detach |
| [fzf](https://github.com/junegunn/fzf) 0.63+ | Interactive menus and pickers |

Feature-specific tools are installed separately: `curl` for issues and PR
watches, with a [token](docs/config.md#forges) for private repositories; the agent or
command you want to run;
and `happy` plus `curl` for Happy sessions. Simulator use requires macOS with
Xcode, `xcrun simctl`, and an installed simulator runtime.
Service installation uses launchd on macOS or a systemd user session on Linux;
without one, [run the daemon in the foreground](docs/reference.md#daemon).

## Create a workspace

Register a local repository or Git clone URL once, then create a task. Each
repository's workspaces (and its clone, for URLs) live under `~/shoal/<name>/`;
`shoal move root <dir>` moves them all to a new root:

```sh
shoal repo add /path/to/project --name my-project
shoal repo setup my-project   # Have an agent write its config, committed or local
shoal config setup            # Have an agent write the global config for this machine
shoal start my-project --branch fix-login
shoal start --in 3 -a codex
```

Use `shoal start my-project` to pick a new or existing branch, an open issue or an open PR, or pass it explicitly:

```sh
shoal start my-project --branch fix-api --base feature/api  # Branch from feature/api
shoal start my-project --branch fix-api --base origin/feature/api  # Fetch the pushed branch first
shoal branch base set feature-api --workspace 4        # Record that workspace 4 builds on feature-api
shoal split fix-typo --workspace 4           # Create a child workspace for work in its own PR
shoal start --copy 4 --fork                  # Try another approach from workspace 4's current files
shoal start --copy 4 --checkpoint 3          # Start from workspace 4's checkpoint 3, live or removed
shoal start my-project --branch origin/feature/api --agent codex  # An existing branch opens
shoal start my-project --branch quick-fix --path ../quick-fix
shoal start my-project --branch fix-login --name "Login retries"  # Name it apart from its branch
shoal adopt ../existing-worktree        # Take ownership, including automatic cleanup
shoal adopt --copy ../existing-worktree # Copy work into Shoal's default directory
shoal branch rename 3 fix/login              # Rename its branch; its name stays
shoal rename 3 "Login retries"          # Name the space in lists and tabs
shoal rename --request 3                # Ask its agent to choose the name
shoal rename                            # Pick a workspace or chat, then type its name
shoal start https://github.com/owner/repo/issues/68 # Create from an issue
shoal start https://github.com/owner/repo/issues/34  # Start on an issue, PR, or branch link
shoal contribute https://github.com/owner/repo/issues/68  # Fork when you cannot push; remove the clone afterwards
shoal link https://github.com/owner/repo/issues/34 assigned  # Assign an issue to the current workspace or chat
shoal unlink issue assigned                # Remove the assigned issue
shoal watch pr 505                         # Watch an explicit PR
```

Inside a terminal host that config enables, such as Herdr or
[Rex](https://www.superlogical.com/rex), `start` opens a new tab or window after
your choices; use `--here` to run in the current pane, `--focus` to switch to
the new place, or the host's `enabled = false` to treat it as a plain terminal.
`[terminal] default_host` names the host that launches from other terminals go
to. With the default, zmx, an agent that starts with an issue or arguments runs
in a detached zmx session that `shoal attach` reaches. See
[Agents](docs/reference.md#agents) for each host's settings.

Use `--agent claude` for Claude Code, `opencode`, `pi` or `grok` for those
tools, or `--agent happy-claude`/`happy-codex` for
a detached [Happy](https://github.com/slopus/happy) session that appears in the
Happy app. Interactive `shoal start` uses `--agent` or picks an installed agent.
Starts on an issue also use `agent.default = "codex"` from the repository or
global config. Edit `~/.config/shoal/rules/worker.md` to add instructions for every
workspace agent, `rules/chat.md` for every chat and `rules/work.md` to replace
the practices every built-in mode lists. Agents read a
repository's own rules from its `AGENTS.md` or `CLAUDE.md`; the same rule
files in a repository's `.shoal/` directory add instructions only Shoal's agents
need.
New workspaces branch from the repository's default branch unless `--base REF`
is supplied to `start`. With shell integration, `shoal start` enters the
workspace unless its agent runs in a zmx session.

Agent admission defaults to starting agents and CI jobs one at a time while
measured memory and disk leave room; `machine.load = "light"`, `"balanced"`
or `"full"` says how hard to push the machine. `shoal queue` shows what holds
each waiting entry. Set a fixed machine-wide limit
(`shoal config set machine.max_agents 3`) or `none` to disable queuing, and
limit one repository's agents with `agent.max_running` (`--repo my-project`). `shoal start` queues agents past the effective
limit and starts them as slots free; `--now` starts one anyway, and `shoal rm
<name>` drops a queued one. `--add-blocker <space|issue>` holds an agent
until that work is done and starts it on the finished branch; an issue's agent
is also blocked by the issues blocking that issue. See the [agent queue](docs/reference.md#agent-queue).

Shoal can also run a registered repository's CI jobs on this machine:
`shoal config set runner.enabled true --repo my-project` registers a runner on
its Forgejo, and jobs with `runs-on: shoal` wait in the same queue, run in a
workspace at their commit that starts from the seed build, and go once they
end. See the [runner](docs/reference.md#runner).

## Everyday commands

After [adding an AI tool](docs/reference.md#agents), run
`shoal start my-project --branch fix-api --agent droid` or `shoal start <issue-url>`.

An agent starts in a mode, which decides what it is told and whether it gets a
workspace or a chat: `worker`, the default, implements its subject in a
workspace; `chat` discusses the repository; `plan` settles an issue's plan
with you; `orchestrate` starts a master session, a chat without a repository
that starts and steers the work of every repository for you. Without `--mode`,
`shoal start` asks which on a terminal. Configure your own under
`[modes.<name>]` and `[rules.<name>]`; see
[modes and rules](docs/reference.md#modes-and-rules).

```sh
shoal                         # Inbox when something needs you, else workspaces; Tab switches
shoal repo                    # Interactive repository menu
shoal inbox                   # Questions, approvals, agents needing input, failures, ready work, demos; --print to list
shoal list                    # Workspaces, chats and queued agents; --print to list
shoal queue                   # Agents waiting for a slot under the agent limits; start or cancel one
shoal list --chats            # Running chats
shoal list --repo my-project  # Only one repository's workspaces, chats and queued agents
shoal list --status needs-input,failed  # Only rows with these statuses
shoal status 3                # A workspace's or chat's activity, issue and PR state; a workspace's changes and resources
shoal status pr 12            # Workspaces and chats that link PR 12; also issue, ci or resource
shoal status                  # Outside a space: the daemon, unread notifications and cleanup failures
shoal config show 3           # Effective settings and the source of each value
shoal config reference        # This release's configuration reference
shoal config set agent.default codex
shoal config set cleanup.auto.enabled true cleanup.auto.idle_minutes 30  # Saved together
shoal config set agent.default claude --repo my-project
shoal config unset agent.default  # Restore the default
shoal cd 3                     # Enter a workspace; omit it for a picker
shoal run --workspace 3 -- cargo test
shoal edit 3                   # Open in $VISUAL/$EDITOR, or [commands] edit = ["zed", "{path}"]
shoal run check --workspace 3              # With [commands] check = ["cargo", "test"] in config
shoal run                     # List configured commands, arguments, and source layers
shoal open 3 --app claude          # Open in the Claude desktop app (macOS)
shoal open 3 --app codex           # Open in the Codex desktop app
shoal attach 3                 # Reattach to the workspace's agent session; Ctrl-\ detaches zmx
shoal demo                     # Pick a running demo an agent started and open it
shoal demo start web           # Start the configured demo `web` for the user to try
shoal start --in 3 -a happy-claude  # Detached Happy session, visible in the Happy app
shoal start my-project --mode chat  # Discuss the repository with an agent in its checkout
shoal start --in 3 -m chat     # Review a workspace's live work in a chat in its worktree, beside its agent
shoal start --in 3 -a claude   # Start Claude Code as workspace 3's own agent, on its issue
shoal start --mode plan --issue  # Settle the plan of an issue that needs one, in a chat
shoal start --mode orchestrate   # Manage the spaces of every repository from one chat
shoal rename 5 "Login design"  # Rename a running chat
shoal rename --request 5        # Ask its agent to choose the name
shoal diff 3                   # Changes since the branch's fork point
shoal conflicts --workspace 3  # Whether the branch merges cleanly into main
shoal alerts                  # Conflicts, finished agents, and removals you missed
shoal alerts send "PR #12 is ready to merge"  # Notify the user from a workspace
shoal alerts send "New layout" --attach shot.png  # Send the user a file with it
shoal progress "Reproduced; the cause is the query limit"  # Set the agent's status line
shoal progress --space 3  # Its status line and plan steps
shoal messages send "Stop the dev server" --space 3  # Message the agents of a workspace or chat
shoal messages send "Crash on launch" --attach shot.png  # Attach a file the agents can read
shoal messages send --all "Network is back"  # Message every agent, running or stopped
shoal log 3              # The space's log: creation, agent, PRs, ready, done; --follow keeps printing
shoal log                # Outside a space: every space's entries, daemon starts and stops, reloads, cleanup
shoal files 3                 # The files attached to the workspace
shoal files 5                 # The files a chat's agent attached
shoal questions               # Pick a question and answer it
shoal questions 3 --answer 2  # Answer question 3 with its second choice
shoal stop 3                  # Stop agents and commands here and below; they stay down until shoal restore
shoal stop 3 5                # Stop several; the picker selects them with Tab or Ctrl-A
shoal stop --all              # Stop every workspace and chat; a reboot saves agents too
shoal stop --interrupt 3          # Cancel its agent's turn; the agent keeps running
shoal stop --leftovers        # Stop what this workspace's commands left running
shoal stop --processes 3          # Stop its processes Shoal does not track; agents keep running
shoal restart --all --agent claude  # Restart Claude agents in new terminals, such as after an update
shoal restart --all --sequential    # Restart one at a time, idle agents first
shoal restore                 # Pick stopped work, stopped chats and removed workspaces to restore
shoal restore --all           # Queue the restore of stopped agents, restore stopped chats; report stopped commands
shoal restore 3               # Continue its stopped agent, or the turn shoal stop --interrupt ended
shoal restore 3 --now         # Restore its stopped agent at once in the background
shoal restore 3 --focus       # Restore its stopped agent in this terminal
shoal restore 5               # Restore a chat Shoal saved, continuing its session
shoal done 3                  # Mark finished and request safe cleanup
shoal done --cleanup 3 # Request cleanup despite [cleanup.done] enabled = false
shoal done --attach report.md  # Hand back files, such as an investigation's report
shoal done --summary "No leak; the cache is freed on drop" # State the finding in the completion notice
shoal done --integrated 3 # Another branch has its work under new commits
shoal done 3 --undo                # Withdraw a recorded done
shoal rm 3                    # Remove the workspace
shoal rm 3 6                  # Remove several; the picker selects them with Tab or Ctrl-A
shoal cleanup                 # Pick leftover workspaces, kept branches and caches to remove
shoal checkpoint 3            # Keep HEAD and unsaved files to return to later
shoal restore <id>            # Open a removed workspace again
shoal restore 3 --checkpoint  # Pick one of the workspace's checkpoints
shoal restore 3 --checkpoint 3 # Return the workspace to checkpoint 3
shoal move ssh other 3 # Move the workspace or chat to another machine; its agents continue there
shoal move export 3 | ssh other shoal move import # The same through any pipe
shoal move export --repo app --all -o moving # Export every workspace and chat of app, a file each
shoal move export 3 --keep -o copy.shoal # Export a copy and keep fix-login running here
shoal move ssh other 3 --keep # Copy fix-login to another machine and keep it running here
shoal move import moving           # Import every space exported to a directory
shoal pr open                 # Push, then link the branch's open PR or open one
shoal pr edit --draft         # Change the linked PR's text, labels, reviewers or draft
shoal pr reviewers            # Users and teams who can review the linked PR
shoal pr comment --body Done  # Comment on the linked PR; close and reopen work too
shoal pr comment --attach demo.mp4 # Upload a file and show or link it in a comment
shoal pr comment --edit 812 --body-file fix.md # Replace the body of a comment you wrote
shoal pr resolve 812 --body-file why.md # Reply in a review thread, then resolve it
shoal pr react +1 --comment 812 # React to a comment; --remove takes it back, issue react too
shoal pr review 42 --comment --line src/a.rs:7 'Why?' # Review with line comments; --approve is yours
shoal pr update --method rebase # Update the linked PR's branch from its base on the forge
shoal pr merge 42 --method rebase # Merge a PR yourself; agents cannot
shoal pr merge 42 --auto --method squash # Merge it once checks pass; --cancel-auto withdraws it
shoal pr delete-branch 42     # Delete the branch of a merged or closed PR on the forge; agents cannot
shoal pr revert 42            # Open a PR that reverts merged PR 42
shoal issue ls --label bug flaky # Search issues by text, label, author and state; pr ls too
shoal issue ls --repo team/tool crash # Search another repository's issues
shoal issue close             # Close the linked issue; edit, comment, reopen too
shoal issue open --title Bug  # Open an issue, recorded as opened; --link also assigns it
shoal issue open --repo team/tool --title Bug # Open it in another repository
shoal repo labels                  # The repository's labels and what each one means
shoal forge capabilities            # What the repository's forge supports, such as resolving threads
shoal forge reactions               # The reactions the repository's forge allows
shoal forge api 'repos/{owner}/{repo}/branches' # Any path of the forge's API; agents only read
shoal forge download URL -o shot.png # An image or attached file on the forge, with its login
shoal ci run deps.yml --input level=full  # Start a workflow run on the workspace's branch
shoal ci run https://github.com/team/tool/actions/workflows/ci.yml  # Or paste a workflow's link
shoal ci ls --pr 44               # The runs on a PR's head; --branch and --workflow filter too
shoal view ci 2765                  # A run's jobs and failed jobs' logs, or paste any run's URL
shoal view ci 2765 --job rust       # The whole log of one job, passed or failed
shoal link 42 assigned        # Assign an issue or PR by number (or paste its URL)
shoal link pr 43 assigned     # Assign another PR; all must merge before cleanup after done
shoal link 50 related         # Record an issue or PR the work relates to
shoal link ci 2765 related    # Follow a CI run; watch reports when it finishes
shoal watch                   # Wake on all linked item comments, checks, or closure
shoal view                    # Linked items with state, failed CI logs, comments, and reviews
shoal view pr 44 --no-comments  # One PR, linked or not, without its discussion
shoal view pr 44 --check rust   # Show only this failed CI check's log
shoal ci rerun --check rust   # Rerun a failed check on the linked PR; cancel stops running ones
shoal ci rerun 2765 --failed  # Rerun one run's failed jobs
shoal ci approve --pr 44      # Approve a fork PR's runs waiting for approval; agents cannot
shoal ready                   # Mark linked items ready for review at HEAD
shoal unlink pr 43 assigned   # Cancel one assigned PR
shoal unlink pr assigned      # Cancel assigned PRs
```

For workspace actions, omit the name to use your scoped or current workspace;
outside one, choose from the interactive picker or pass a target for `--json`
and noninteractive calls. Bare `shoal cd` always opens a picker. `shoal cd -`,
`shoal attach -` and other commands that open, show or run something in a
workspace take `-` for the workspace they last acted on; for `attach`, the
workspace or running chat whose session you last detached from, whichever
command attached it.
Use `shoal <command> --help` for options.

To move changes between your workspace and the default branch:

```sh
shoal repo sync                    # Fetch and fast-forward the default branch
git rebase main               # Or git merge; use your repo's branch name
shoal merge                   # Merge your branch into the default branch; no remote needed
shoal merge --push            # Then push the default branch, after asking, instead of opening a PR
shoal unmerge                 # Undo your branch's latest merge while it is unpushed
```

To have an agent review a workspace's committed changes or a PR:

```sh
shoal review run 3            # Queue a one-shot, read-only review; findings reach the agent
shoal review run <pr-url>     # Review a PR in the workspace that owns its branch
shoal review ls             # Reviews, newest first, with unresolved findings
shoal view review             # The latest review in full
shoal review resolve 17.2 false-positive --reason 'The caller checks it'
```

With `[commands] review = ["tuicr", "-r", "{diff_base}..HEAD"]` configured,
`review run` asks whether to run it or an agent. See
[local reviews](docs/reference.md#local-reviews) for reviewers, outcomes and the
[review command](docs/reference.md#configured-commands) for uncommitted review.

CLI agents and chats run in sessions of a terminal host, by default
[zmx](https://github.com/neurosnap/zmx), that keep running when you detach, with
Ctrl-\ in zmx or the host's own control elsewhere, or close the terminal;
`shoal attach` returns to one, such as a zmx
session or a Herdr tab, by its workspace's or chat's number or ID. Happy sessions run detached, log
to a file Shoal names, and stop with `shoal stop`. Codex and Claude run with
their own sandbox and approval settings; Shoal turns on network access in Codex's
sandbox so its commands reach the daemon. Use `shoal run --workspace 3 -- codex`
for a custom invocation. Shoal's resource scope is cooperative, not a filesystem sandbox.
Apps that launch their own processes can request `shoal internal env 3 --json`
and add the returned variables to each child's environment. Revoke its scope
with `shoal internal env 3 --revoke <token>`. See
[workspace scope](docs/reference.md#scoped-workspace-commands) for token lifetime
and cleanup behavior.
The agents Shoal starts get JSON from `shoal` without `--json`; give an agent
you start yourself `SHOAL_OUTPUT=json` for the same, and pass `--text` for
human output.

Agents' issue and PR changes need an `agent_token` for the forge host: a
separate agent account's token, or `{ same_as = "user_token" }` to make them
with your login. A [Git profile](docs/reference.md#git-profiles) sets the
agent's commits:

```toml
[agent]
git_profile = "agent"

[machine.forges."git.example.com"]
user_token = { cmd = ["security", "find-generic-password", "-s", "forgejo", "-w"] }
agent_token = { cmd = ["op", "read", "op://dev/agent/token"] }  # Or { same_as = "user_token" }
```

See [agent accounts](docs/reference.md#agent-accounts).

Memory overload protection stops tracked agents by default while retaining their
workspaces and leases; before that, Shoal tells running agents that memory pressure is high. To opt out, run `shoal config set machine.overload.memory.enabled
false`. See [overload protection](docs/reference.md#overload-protection)
for thresholds and timing. When disk space runs low, the daemon removes
workspaces idle cleanup would remove; if space stays critical, it stops tracked
executions. Agents restore once space returns; `shoal restore` restores the rest. Stop agents manually with `shoal stop`:
they stay down, holding no agent slot and no workspace from cleanup, until
`shoal restore` continues them, as it continues an agent that exited on its own,
such as one you quit. To take agents down briefly, use `shoal restart`. `shoal
restore --all` queues the restores to start as the agent limits allow; `--now`
starts them at once in the background, and `--focus` restores one agent in this
terminal. Agents whose terminal closed, or that a logout or reboot
stopped, come back through the same queue on their own. Codex and Claude automatically continue
the workspace session after load recovers; an agent's `resume_command` overrides
its restore command or enables recovery for other agents.

## Share resources

From a managed workspace:

```sh
shoal acquire port web --reason "Development server"
shoal catalog                 # What each kind offers: configuration, devices, runtimes
shoal leases                  # Every kind's leases and usage
shoal release port web

shoal acquire sim --queue     # macOS; requires configured simulator profiles
shoal release sim

shoal acquire resource signing --queue
shoal release resource signing
```

Use the returned port or simulator UDID. Reservations and leases belong to the
workspace and survive command exit; release them when finished.

Install a packaged global config with `shoal config install <name>`;
`shoal config install --help` lists the available names.

Put project defaults in `.shoal.toml` or `.shoal/config.toml`. For example:

```toml
[ports.web]
port = 3000
env = "PORT"

[resources.signing]
capacity = 1
requires_approval = true
approval_lifetime = "lease"    # Or "workspace"

[resources.server]
kind = "repo"
repo = "saldoir-server"       # Registered repository name or remote URL
```

Acquire a related repository with `shoal acquire repo server`. Use its
checkout path in a project override, for example:

```sh
server=$(shoal --json acquire repo server | jq -r '.repository.path')
SALDOIR_SERVER_REPO="$server" Scripts/sync-contract.sh
shoal release repo server
```

Agents request protected resources with `acquire --reason "purpose"`. Review
requests with `shoal approvals`, then use `shoal approvals --approve <S.N>` or
`shoal approvals --deny <S.N>` from an unscoped terminal, where `S.N` is the
requesting space's number and the request's, such as `3.2`.

Select a named Git identity with `git_profile = "work"` in that file, or use
`shoal start my-project --branch feature --git-profile work`. Define the profile
in global config; see [Git profiles](docs/reference.md#git-profiles).

New workspaces get the repository checkout's ignored local files, such as `.env`;
set `[setup] copy_ignored = false` in that file to turn this off. The same file can name a
`setup.command`, run automatically on workspace creation or explicitly with
`shoal setup [workspace]`, and hooks around setup, assignment completion,
agent exits, removal, and resource acquisition/release, for example to open a tmux session or prepare an
external device. See [setup and hooks](docs/reference.md#workspace-setup-and-hooks)
and [resource hooks](docs/reference.md#resource-hooks) for configuration and failure behavior.
A `[setup.seed]` table keeps a build of the default branch that new workspaces clone, so
they rebuild only what their branch changed; see [seed builds](docs/reference.md#seed-builds).
`[setup] presets = ["cargo", "bun"]` sets that up for the tools a repository uses; see
[build tool presets](docs/reference.md#build-tool-presets).

Use `[env]` to give tracked executions and hooks workspace-specific variables. Values
substitute `{workspace}`, `{workspace_path}`, `{repo}`, `{branch}`, reserved
`{port.NAME}` values and [shared cache](docs/reference.md#shared-caches) directories
`{cache.NAME}` once; inserted text is not evaluated. For example:

```toml
[env]
AGENT_BROWSER_SESSION = "{workspace}"
AGENT_BROWSER_PROFILE = "{workspace_path}/.agent-browser"
GOCACHE = "{cache.go}"

[caches.go]
scope = "shared"
```

Use `[secrets]` in global or local config to give agents, or only
tools such as `agent-browser`, values fetched from your secret store; see
[agent secrets](docs/reference.md#agent-secrets).

## Cleanup

`shoal rm` removes workspaces and chats with their resources and the workspaces
below them, prompting once, and only before it deletes uncommitted work, takes
workspaces below a chat or stops a chat's agent. It returns once its checks pass
and removes a workspace in the background, notifying you if that fails; `--wait`
waits for it. It deletes the branch too unless the branch has commits that are
on no remote and not on the default branch; Shoal deletes such a kept branch once
they are, and `shoal cleanup branches` deletes kept branches, several at a time, with
their commits. `shoal stop` keeps the workspace.

Automatic cleanup removes idle, clean, pushed or merged workspaces after 10 minutes,
merged workspaces after 4 idle hours whether or not their agent ran `shoal done`,
and forgets workspaces whose directory you deleted yourself, keeping the branch.
A chat whose agent stopped goes by the same rules.
`shoal cleanup` lists what Shoal left behind: those spaces, kept branches, and
shared caches nothing is using, with why it keeps the other spaces. On a terminal
it opens a picker, where Tab selects several, and removes what you pick.
`shoal cleanup spaces`, `branches` or `caches` acts on one kind, and `shoal status`
shows why it keeps one workspace.
Removal keeps the workspace's HEAD and unsaved files in a checkpoint, except a
disposable workspace's, as `shoal checkpoint` does and Shoal does when an
agent exits or is stopped. Until the checkpoint expires, after 30 days by default,
`shoal restore` picks a removed workspace among what it restores, and
`shoal restore <id>` opens one again with its links and the unsaved files the
checkpoint kept, and restores the agents the removal stopped; `--no-agents` keeps them
saved for a later `shoal restore`, and `--forget` forgets them. A checkpoint keeps
HEAD only when the unsaved files are too large or include a nested repository or
changed submodule; see [Checkpoints and restore](docs/reference.md#checkpoints-and-restore).
`shoal restore <workspace> --checkpoint <number>` returns an existing workspace
to one of its checkpoints. `shoal move export <workspace> -o <file>`
removes a workspace and writes its removal checkpoint and record to a file, and `shoal
move import <file>` opens it on another machine or the same one, where `shoal restore`
continues its agents' sessions from the transcripts the file carries;
`--new-sessions` starts them over. A chat moves the same way: the export ends it,
and the import starts it again, continuing its session.
An app hosting an external session can hold its workspace while the session is open:

```sh
shoal acquire hold --workspace 3 --name quay-thread-42 --reason "Quay thread"
shoal leases hold --workspace 3
shoal release hold --workspace 3 --name quay-thread-42
```

Configure idle cleanup in a repository's `.shoal.toml`, or for every repository in
`~/.config/shoal/config.toml` followed by `shoal config reload`:

```toml
[cleanup.auto]
enabled = false
```

When something looks wrong, `shoal inbox` and `shoal alerts` show what Shoal
found and the command that fixes it, and `shoal status <name>` shows one
workspace or chat; see [cleanup](docs/reference.md#automatic-cleanup) and
[recovery](docs/reference.md#recovery) for details.

## Update

```sh
brew update
brew upgrade hn05/tap/shoal
```

Update a downloaded binary or `Shoal.app` with `shoal update`. Managed daemons apply the upgrade once their operations are idle; run
`shoal daemon restart` to apply an upgrade immediately; running agents and
commands reattach to the new daemon. Wrappers from releases before reattachment
are stopped instead; restore them with `shoal restore --all`. The next `shoal`
command updates installed skills; reload `source <(shoal shell init)` in open
terminals.

## Development

See [docs/design.md](docs/design.md) for decisions, the [command reference](docs/reference.md) for behavior and the [configuration reference](docs/config.md) for every option.
Create releases through **Actions → release** ([setup](docs/releases.md)).

Requires Rust at least as new as `package.rust-version` in [Cargo.toml](Cargo.toml)
and the [runtime dependencies](#runtime-dependencies).
The tests run with [cargo-nextest](https://nexte.st), and integration tests also
use Bash, Zsh, and Python 3. Debug builds leave the
user's state directory, config directory and user-level files outside their own,
such as installed skills, to the installed Shoal: run them with a temporary
`--state-dir`, and with a temporary `HOME`, holding `XDG_CONFIG_HOME` if it is
set, to change the config. Tests in a
temporary `HOME` set `SHOAL_DEV_USER_FILES=1` to change user-level files.

```sh
cargo build
cargo nextest run
cargo test --doc
CARGO_BUILD_WARNINGS=deny cargo clippy --all-targets
cargo fmt --check
```
