# Workspaces

This file says how `shoal start` and the commands that maintain a space work
with workspaces, their names, bases and adopted worktrees.

`shoal branch rename [WORKSPACE] BRANCH` renames a ready workspace's checked-out Git
branch while keeping its name, worktree path, `{workspace}`, and owned resources
and settings. Dirty files are preserved;
Git keeps the branch's upstream and reflog. Remotes and open PR head branches
are unchanged. The branch must be free locally and in no other workspace of the
repository; rename does not choose a suffix. The default branch, branches checked out
elsewhere, and reserved names are refused. Scoped callers may rename
their own workspace when no other execution is recorded; unscoped renames
require no recorded executions. The caller keeps its original environment, and
new commands receive the renamed identity. PR watches and acknowledgements stay.
An interrupted rename leaves the workspace failed until `shoal branch rename` runs
again: it first finishes the interrupted rename when Git completed it, or cancels it
when the branch is unchanged, then renames to the branch given. Confirmed-deleted
worktrees can still be removed through normal cleanup.

`shoal rename [SPACE] NAME` gives a workspace or chat a new name, which its
agent may choose to say what the work is. The tabs, windows and blocks Shoal
opens for the space show it, and setting it renames those already open and
those its agents run in, wherever the command runs, while they still show the
label Shoal gave them; one the user named keeps its label. The worktree
directory, branch and zmx session keep their names. Scoped callers may rename their own space and those below it.
`shoal rename --request [SPACE]` sends the space's agent an
[alert](alerts.md) asking it to choose the name and set it; it is
refused while no agent runs there. `shoal rename` without arguments picks a
workspace or running chat and asks for its name.

`shoal start <link>` recognizes issue, PR, CI run and branch URLs. Links select the single
registered repository with the same origin; `-r`/`--repo` selects one explicitly and
must match the link. An unregistered remote offers interactive registration,
otherwise lookup fails with the `shoal repo add` command to run. Lookups use your
[forge login](issues.md#forges).

Issue links and `shoal start <number>` require an open issue and derive
`<number>-<words>` from up to three title words, skipping filler such as “a” and
“the”, within 32 characters; a title without ASCII words gives `issue-<number>`.
Numbers use `--repo`, the current registered checkout or workspace, then an
interactive repository picker. An existing association
reopens its workspace; otherwise the derived local branch reopens as an existing
`--branch` opens. Lookup and ownership failures create nothing. The command starts
its agent as [Agents](agents.md) says. The issue prompt
names the issue's number, title and URL, includes its description when it has
one, and points to `shoal view` for comments and later edits; options after
`--` go to the agent. Codex uses CLI mode for issue prompts.

PR links, and `--pr` with a PR's number or URL, or without one a PR a
terminal picks among the open ones, those carrying the `label` of the mode
`--mode` names where it has one, open its head branch against
the refreshed remote base, reusing an
owned workspace when present, and link the PR once the workspace is ready; a
refused link only warns. A PR from a fork, which has no branch in the
repository, opens in a [workspace at a commit](create.md) at its head,
below the space running `shoal start` or else top-level, named after the PR's title in a
directory `pr-<number>`, fetched from the ref the forge serves it under
(`refs/pull/<number>/head`, or `refs/merge-requests/<number>/head` on GitLab), with
setup and its diff, `{diff_base}` included, compared with the PR's
`origin/<base>`; a forge without such a ref refuses it. `--agent` starts the
workspace's own agent there, which without a branch cannot push or open a PR.
The workspace made for or linked to the PR is reused, so there is at
most one per PR: when the PR's head moved, it moves to the new head, unless it has
commits or changes of its own, which is refused. A workspace's or chat's agent
reuses it only when it is below that space. Branch URLs use
GitHub's `/tree/<branch>`, GitLab's `/-/tree/<branch>`, Bitbucket's
`/branch/<branch>` or Forgejo's `/src/branch/<branch>` route and open the
origin branch, preserving slashes and decoding URL escapes. Setup, hooks and
existing-branch ownership checks apply. These forms launch an agent with `--agent`.
`start [<repository>] --issue <number-or-url>`, or `-i`, remains available with an optional
`--branch`, finds the repository of an omitted `<repository>` as numbers and
links do, and launches an agent only with `--agent`.

CI run links, and `--ci` with a run's URL or the number in it, which names the
run in the repository `--repo` names or else the current one, or without one, on
a terminal, a run picked among the newest of each workflow and ref whose latest
attempt failed, start where the run ran, in any state: a run for a PR opens as
that PR's link does, a run on another branch than the default branch opens that
branch as `--branch` does, and a run on the default branch, a tag or no branch
names no work, so the start begins as a chat in its mode; a mode that starts a
chat starts one on any run. The mode is `--mode`'s or the default, never one a
PR's labels call for. A run refuses `--issue`, `--pr`, `--branch`, `--copy`,
`--in` and a second link beside it. The new or reopened space links the run as
related, so its watch reports later attempts, and its agent starts on the run:
its workflow, number, ref, URL, commit, state and failed jobs, pointing to
`shoal view ci` for their logs. A chat on a run is named
`<workflow> run <number> on <ref>` unless `--name` names it. A scoped caller's
run of another repository is refused as `shoal link ci` refuses it.
`shoal create` takes a run link of a PR or branch the same way.

```sh
shoal repo add /path/to/repo             # Or a Git clone URL; register once
shoal repo add /path/to/repo --name my-project
shoal repo rename my-project new-name
shoal repo share my-project              # Every machine of the hub clones it
shoal repo unshare my-project
shoal repo ls
shoal repo                               # Interactive repository menu
shoal start my-project --branch fix-login
shoal start my-project --branch fix-api -a codex -- "Fix the API timeout"
shoal start https://forge.example/team/repo/issues/34 -- --model fast
shoal cd                                 # Fuzzy picker, even inside a workspace
shoal cd 3                               # Enter through the shell function
shoal cd -                               # Previous directory
shoal run --workspace 3 -- cargo test
shoal edit 3                             # Open in $VISUAL/$EDITOR or the edit command
shoal open 3 --app claude                # Claude desktop app (macOS)
shoal open 3 --app codex                 # Codex desktop app
shoal open --app t3                      # Running T3 Code desktop app; current space or picker
shoal open 3                             # app.default, else the only installed app, else a picker
shoal open 5 --app codex                 # A chat, in the worktree or checkout it runs in
shoal start --in 3 -a claude             # Start workspace 3's own agent, on its issue
shoal start --in -a codex                # The current workspace's, else a picked one's
shoal start --in 3 -a happy-codex        # Detached Happy session for the Happy app
shoal status 3                      # Activity, changes, held resources, issue and PR state
shoal status 5                      # A chat's agent, sessions, issue and PR state
shoal status pr 12                  # Workspaces and chats that link PR 12
shoal status ci 2765                # Workspaces and chats that link CI run 2765
shoal status resource devices       # Workspaces that hold a devices lease
shoal stop 3                             # Stop agents and commands until shoal resume
shoal rm 3                               # Remove; choose what to keep if work differs
shoal rm 3 6                             # Remove several, confirming once for all
shoal rm 5                               # Remove a chat and the spaces below it
shoal rm 3 --wait                        # Return once it is gone, naming a kept branch
```

Bare `shoal` opens an fzf picker on the [inbox](#inbox) when something needs you,
searching for the current workspace's items when it has any, otherwise on the
list of workspaces; `shoal inbox` and `shoal list` open it on
their view on a terminal. Tab cycles the inbox, the workspaces and recent
alerts, which it marks read as `shoal alert list` does; there
Enter enters the alert's workspace (`shoal --help`, or bare `shoal`
without a terminal, prints commands grouped by task and a starting workflow;
commands that only Shoal, its agent hooks and integrations run live under the
hidden `shoal internal`). Workspace processes see only the workspaces.
Its rows, like `shoal list` and workspace pickers, are aligned and marked by what
their status asks of you: ● your turn to answer, review or resume, ◐ an agent
or process works, ○ quiet, ◌ changing state, or ✗ failed. Each shows the
space's number, then names the workspace, its repository when the
rows span several, its status, what its processes and containers use while any run, and the
linked issue's title or CI job, cut with `…` to fit a terminal's width. Usage counts every
process carrying the workspace's `SHOAL_SPACE`: CPU as a percent of one
core since the daemon's sample before, which it takes every ten seconds and
when a workspace's containers start, including children those
processes have waited for, then memory, resident on Linux and the physical
footprint on macOS, with what its [containers](resources.md#containers) use. `shoal status` adds the process count, and `--json` gives
`usage` in bytes and percent. The status is the
state unless ready; for a ready workspace it is, in order of precedence,
`queued (N)` while nothing Shoal started runs there and an agent other than
one asleep there waits at place N in
the [agent queue](queue.md) to start in it, or `starting` once the queue
admits it, each followed by what else it waits for as `shoal queue` shows (another
status names such agents after it, as in `process running, queued (2)`), `stopped` when stopped agents or commands wait for `shoal resume` and the
workspace is neither done nor [blocked](progress.md), `asleep` while its agent
is [asleep](sleep.md), `needs input`, `blocked`
while its progress records what blocks it, `finished` after `shoal done` until
`shoal done --undo` or `shoal resume` withdraws the completion, also while its
work is stopped, `working`, `ready`, the other [agent states](agents.md#agent-state),
`process running` while an agent without a reported state or a command runs,
`parent working` while no agent of its own has ever started there and the
agent of the nearest space above with one works, as when that agent's
subagents work in a child it created, or `inactive`.
`shoal list` and its picker order spaces by status, then number: working
agents and processes first, then what needs you, failures, changes under
way, queued agents, and quiet spaces last.
Branches and paths are in `shoal status`.
Every space, a workspace or a [chat](chats.md), has a number no other space
holds, on any machine joined to the same [hub](hub.md), the smallest free when
it is created; it stays through renames and stops, and a later space may
take it once the space is removed. `shoal status`
shows it, and `--json` gives it as `number`. Every argument that takes a space
selects it by that number or its ID, so `shoal attach 3` and `--space 3` name
space 3, and `-` selects the space the command last acted on; other text is
refused with "a space is selected by its number or ID". A name never selects
a space, so names may repeat. A removed space has no number and is
selected by its ID; while its log keeps the removal, a command that needs
it to exist is refused saying it was removed, and with which repository
when its repository's removal took it. Without a space, a terminal opens a picker that shows the
names. `--add-blocker` is the one argument where a bare number names an issue
instead.
The header names the keys the list binds, most used first, on as many lines as
the terminal needs. Enter enters the selection; Ctrl-T attaches, Ctrl-F shows
the diff, Ctrl-V lists the workspace's linked issues and PRs in its repository,
where Enter shows one with `shoal view` and Ctrl-O opens it in the browser with
`shoal view --web`, Ctrl-A adds, Ctrl-E runs Claude/Codex CLI, starts a Happy session,
opens Codex/T3 apps, or runs a shell command, Ctrl-O shows status, Ctrl-S stops,
Ctrl-D deletes. Each action returns to your shell.
`shoal list` with `-p`/`--print`, like a filter, `--json` or output that is not a terminal,
prints the list instead: workspaces and [chats](chats.md), each chat with its
agent, and each agent waiting to start in a workspace the list has no row for,
such as a new one, with its place in the queue and what it starts, each space followed by the spaces below it; `list --chats`
lists only the chats. `list --repo <repository>`, with or without
`--chats` or `--status`, lists only that registered repository's workspaces,
chats and queued agents. `list --status <status>`,
repeated or comma-separated, lists only rows with one of those statuses, spelled
in kebab case (`needs-input`, `process-running`, `queued`).
A chat's status is `asleep` while its agent is [asleep](sleep.md), else
`stopped` until it is restored, or `finished` after
`shoal done` or `blocked` while blocked, then, in order of precedence,
`needs input`, `blocked`, `finished`, `working`, `ready`, its agent's other
[states](agents.md#agent-state), or `process running` before its agent reports one.
<a id="inbox"></a>The inbox lists what needs you, each with its workspace: threads whose
turn is yours and pending approval requests that no coordinating agent decides, each with its
[number](threads.md), including issue
[proposals](approvals.md#approval-for-agent-issues-and-prs) whose workspace is gone, workspaces and
running chats whose agent needs input, blocked workspaces and chats with the
first of what blocks them, failed
workspaces, workspaces and chats whose agent is [asleep](sleep.md) with why,
what wakes it and the commands that wake it now, [ready marks](completion.md#ready-for-review) of
spaces whose agent is not working, one row per marked item, running [demos](demos.md), Git worktrees under a repository's
Shoal directory that no workspace owns, automatic cleanup failing other than in
a workspace, and repositories whose default branch has failed to
[sync](repo.md) for 30 minutes. There Enter shows a thread and reads your reply, approves or denies a request after
asking which, refusing one that replaced the request listed, attaches to the agent that needs input or is asleep, opens a blocked space's actions, shows the status of a failed
workspace, and shows a ready PR or issue with `shoal view` (`shoal status` for a
mark on the workspace itself), opens a demo, adopts an untracked worktree
with [`shoal adopt`](#adopting-a-worktree), shows Shoal's status, as `shoal status`
outside a space does, for failing cleanup, and retries a failed sync with `shoal repo sync`; Ctrl-T, Ctrl-O, Ctrl-F and Ctrl-V act on the row's
workspace and Ctrl-A adds one. `shoal inbox` with `-p`/`--print`, like `--repo <repository>`,
`--json` or output that is not a terminal, prints it; `--json` gives each item's
`kind` (`thread`, `approval`, `needs_input`, `failed`, `asleep`, `ready`, `demo`,
`untracked`, `cleanup` or `sync_failed`) with its `thread`, `request`, `workspace`, `demo`,
`worktree` (`repository_id` and `path`), `error` or `sync`, and a ready item's `mark`. A workspace
process gets only its own items, never untracked worktrees, failing cleanup or failing syncs, printed, and none after clearing its scope
token.
Bare `shoal repo` opens the same kind of list over registered repositories
(without a terminal it prints `repo` help): Enter adds a workspace, Ctrl-A
registers a path or URL, Ctrl-R renames, Ctrl-O shows the local config, Ctrl-D deletes.
`repo rename` asks for an omitted name. Commands that take a registered
repository and omit it use the one of the current registered checkout or
workspace, then a repository picker.

Commands acting on one workspace use an explicit target, given as an argument or
with `-w`/`--workspace`, or `-s`/`--space` for a command that also acts on chats,
otherwise the caller's
scoped workspace or the workspace containing the current directory, then an
interactive fzf picker; a deleted current directory provides no workspace context.
The picker leaves out spaces without what the command acts on, such as an
agent session for `attach` or a running agent or command for `stop`, and reports
when none is left. Spaces the command would refuse for their state, or for
`setup` running work, come last with the reason the command would give in place
of their status. The daemon of the machine each space runs on judges it by the
request the command sends for it.
`cd`, `attach` and the commands that open, show or run something in one space
remember, across terminals, the spaces each acted on, workspaces and chats
alike, and take `-` as the target for the latest one other than the current
workspace and the space the command runs in; `cd` also remembers the space it
leaves, and `attach` the space of each agent session it or a launch such as
`start` or `claude` showed here and left running, also on another joined
machine, and of each place a focused `start` opened. `-` passes over
removed spaces, and `attach -` over chats that no longer run; a command
that acts only on workspaces keeps a chat named to it out of its history. Scoped callers keep no such history.
Invalid explicit targets and other invalid arguments fail without fallback,
before any prompt for the omitted ones; without a
current workspace, noninteractive and JSON calls require a target. Scoped callers
remain confined to their own space, a workspace or [chat](chats.md), and the spaces below it
([child workspaces](create.md) and those a chat below opened): without a target, a scoped command acts on the workspace below
whose directory it runs in, otherwise on the caller's own space, and agent hooks
always report for the caller's own. Another omitted required argument that
names an existing record opens a picker of those records, under the same
noninteractive rule; the refusal names the missing argument and how the command
line passes it. Leaving a picker without a choice exits with status 130
and no message, and a command whose output reader closes early, such as `head`,
exits with status 141 and no message. Bare `shoal cd` always opens a picker;
all-workspace listings and `--all` retain their scope. A command that accepts
`-A`/`--all` lists it as the last picker entry, "All workspaces", and as the first
completion of its workspace argument. `config show` checks the
registered checkout before the picker, as [Inspect effective configuration](config.md#inspect-effective-configuration) describes.
`shoal start` offers repositories in most-recently-used order, then
a new-branch prompt or a picker of existing branches, open issues or open PRs; a
chosen issue or PR opens as its link would. Noninteractive and JSON calls never prompt;
management commands support JSON output, while executed commands keep their
own stdin, stdout, stderr, and exit code.

`shoal status [target]` summarizes one space, a workspace or a [chat](chats.md),
running or stopped, which the target names, else the current one: its state, a
workspace's changes since its fork point and what it holds, its agent sessions,
each session its agents reported with the path of its transcript on this machine,
whether each assigned issue is open or closed,
and each watched PR as the forge reports it now: open, merged or closed, merge
conflicts, each CI check's result, which is `waiting` while no runner has taken
its job where the forge tells that apart, the CI runs on an open PR's head that
[wait for approval](ci.md) in `waiting_runs`, and a review state from each
reviewer's latest approval or change request. A failed lookup reports its error
on that item without failing the command. Status counts the [threads](threads.md)
the space opened that wait for their holder in `waiting_threads`, the approval requests that wait for a
decision, the space's own and those it decides, in `pending_approvals`, its
[attached files](files.md) in `attached_files` and its unread alerts in
`unread_notifications`. When a step
of the daemon's automatic cleanup fails in the workspace, status says so in
`cleanup_error`. `Last sync` shows when the repository's
default branch last [synced](repo.md) and why the syncs since then failed,
in `sync` (`synced_at`, `failing_since` and `error`). It lists the files the
last check of HEAD against the default target found
[changed on both sides](branches.md#conflicts), from a watch or `shoal conflicts`, in
`changed_on_both_sides` (`target`, `target_commit`, `head` and `files`). Status also
names the first condition that keeps the space from automatic cleanup, holds,
threads it opened that wait, a pending PR proposal or push request, a running chat in it, queued agent launches, the CI runs of a merge it follows and assignment state before commands, leases and Git state, or the idle delay
when nothing does, in `cleanup_reason`. It lists the chats in the worktree, in
`chats`. A chat's status leaves out what needs a worktree: its
branch, changes, setup, executions, leases and sync; it names the
workspace it runs in, as `shoal list --chats`, the inbox and the menu do with
`in <workspace>`. `--json`
returns the same data, with `kind` (`workspace` or `chat`) and the space's
record in `workspace` or `chat`, PRs in `prs`, the issue in `issue_status` and sessions
in `sessions`, or a failed `zmx list`, or a session zmx cannot reach, in `sessions_error`. `transcripts` lists every
session the space's agents reported, with `agent`, `session`, the `execution` a
workspace's agent ran as, whether the agent runs it now in `current`, its
transcript in `path` and all its files, the transcript included, in `files`,
which Shoal finds where the agent's tool keeps them and never reads, and
for a session a [handoff](chats.md#convert-a-chat-or-a-workspace) started, the
`agent`, `session` and `integration` it took over from in `continued_from`. A session
whose files are not on this machine, or of an agent whose tool Shoal does not
know, has no `path` and no files, and one whose files could not be looked for
says why in `error`; sessions the agents' hooks never reported, such as those
before a `/clear`, are not listed. Space records from `list --json` and `status --json` name
their `kind`, `workspace` or `chat`, and where the space's agent works in `workdir`, with its
`path` and its own `kind`: `worktree`, with the `workspace` that owns it, for a workspace and
for a chat in a workspace's worktree, `checkout` for a chat in its repository's registered
checkout, or `root_dir` for a chat without a repository. They include
the assigned issues in `links.issues`, first assigned first, assigned PRs in
`links.prs`, and items recorded as opened or related in `links.opened` and
`links.related`, each item with `kind`, `url` and `title` when known; an acknowledgement without a PR URL leaves
`links.prs` empty. `status` lists opened and related items too. An issue, PR or
CI run URL, `pr`, `issue` or `ci` with a number or URL, or `resource` with a pool
or member name shows every space that links it or workspace that holds it instead, as a JSON array; a number must be linked in only one repository.
Scoped callers find only their own workspace and its child workspaces.

## Branch and workspace names

`shoal start <repository> --branch <name>` opens the branch as `git switch`
does: an existing local branch, or `remote/branch`, opens, and any other name
creates a new branch under exactly that name, which a branch it would collide
with refuses rather than suffixing it. A branch that already has a workspace
takes the start into that workspace, as [`--in`](agents.md) does, refusing what
only a new workspace takes. A branch checked out elsewhere is refused. Names
Shoal derives, such as an issue's, keep reopening their branch or workspace as
the [issue start](workspaces.md) describes. A start that
names no branch, issue, PR or copy, including the picker's
"Create a new branch" and a start without a terminal, begins as a
[chat](chats.md) in the chosen mode, `worker` by default, under the chat's
default name; the mode's agent is told to make it a workspace with
[`shoal convert workspace <branch>`](chats.md#convert-a-chat-or-a-workspace) once it
knows the work, before it changes any files. Flags that only a new branch
takes, such as `--base`, are refused there.
For a new branch `--base` is its starting ref; for an existing one it is the ref
`diff` and `{diff_base}` compare against instead of the default branch, for new
worktrees only. The picker
queries remotes for current branches. Local branches take precedence and remain
unchanged; remote selections fetch and create tracking branches, or fast-forward
a matching local tracking branch without discarding ahead commits. Ambiguous
remotes or unrelated local branches fail. A ready Shoal workspace reopens without
setup/hooks or refresh; other checkouts block creation, including checked-out `main`.
Full `refs/heads/...` and `refs/remotes/...` selectors disambiguate names.
A space's name is the label lists, tabs and notices show: a single line of 1–40
characters, any printable ones, with surrounding whitespace trimmed and inner runs
collapsed to one space; control characters and bidirectional overrides are refused,
as is a longer name someone sets, while a name Shoal derives is cut to 40 characters
ending with `…`. Names may repeat across spaces and repositories, never select a
space, and name no file, directory, variable, session or branch, so renaming one
moves nothing. A new space takes the first of: `--name`; for a CI job's
workspace, the job's name; the title of the issue or PR it is started on or
linked to; for a workspace, the branch as given, or without one the base and the
short commit, such as `main at 84d4405`; for a chat, `<repo> <mode>`,
`<workspace name> chat` in a workspace's worktree, or the mode, such as
`orchestrate`, for a master session. Only `shoal rename` changes it later: a
branch rename leaves it unchanged, and `shoal restore` gives a workspace back
the name it had. Its worktree directory is named from the branch with
every character other than ASCII letters, digits, `-` and `_` replaced by `-`,
leading `-_` dropped, cut to 64 characters, falling back to `workspace`, and
suffixed `-2`, `-3`, ... while another workspace of any repository has a directory
of that name; it never moves. Directories default to the repository's
Shoal workspace directory;
`start --path <dir>` selects an exact new directory for this workspace only (relative
to the current directory; `~/` allowed). Existing paths and overlaps with Shoal
state, checkouts, workspaces, or another repository's reserved directory are refused.
Reopening accepts its current path but cannot relocate it. Custom locations have
the same setup, ownership, and cleanup rules as default locations.

For new branches, names taken by a local branch, known remote branch, branch
namespace, or retained Shoal record get `-2`, `-3`, etc. on the leaf; when an
ancestor blocks it, that component is suffixed (`feature` makes `feature/topic`
into `feature-2/topic`). Names Git reads as a revision
instead of a branch (`HEAD`, `@`, or entirely hexadecimal names of exactly 40
or 64 characters, a full object ID in either object format) are reserved:
creation suffixes them, while opening an existing branch is refused. Nested components remain literal.
Suffixes never change the derived workspace name.

## Adopting a worktree

`shoal adopt <path>` registers an existing linked worktree in place
and enters it with shell integration. The path is relative to the current directory
(`~/` allowed) and must name its root. `--repo` selects the registered repository;
otherwise it is the one whose checkout owns the worktree, else the only one sharing
its `origin` remote, else one chosen interactively. It is named after its local
branch; ownership conflicts fail. Main checkouts, detached or locked worktrees,
and paths overlapping protected locations are refused. Repeating adoption reopens
a verified ready workspace; it cannot repair a moved managed worktree.
At a workspace's recorded path, when its worktree exists but Shoal cannot verify
it owns it, adoption reclaims it: check the worktree yourself first, since adopting
it re-marks a linked worktree of the recorded repository on the recorded branch
that no other workspace owns, then restores the verified worktree to ready. A failed
setup is not an ownership failure; adoption refuses it.

Adoption preserves commits, dirty files, and Git settings without fetching, setup,
or post-setup hooks. It takes full Shoal ownership with normal cleanup, including
the pre-remove hook. Disable automatic cleanup
in repository configuration before adopting work that should stay indefinitely.
Use `shoal setup` explicitly when setup is wanted.

Pass `--copy` to create a new branch worktree under the repository's Shoal
directory, copy the source worktree's files into it, and leave the source in
place. The copied worktree is adopted without setup and receives the next
available branch suffix when its source branch is already checked out.

## Base branch

New branches start from the repository's default branch: `origin/HEAD`, or the
sole remote's HEAD without `origin` (several remotes without `origin` are
ambiguous). A missing symbolic remote HEAD is discovered with `ls-remote` and
cached; update it with `git remote set-head origin --auto`. Without remotes, the
registered checkout's current branch is used; a detached checkout needs `--base`.

New branches start from the local default branch as the [sync](repo.md)
left it; creation never fetches it. `--base REF` starts from any locally
resolvable commit, fetching a remote branch it names first; the resolved base is
recorded for `shoal diff`.
Existing-branch workspaces diff against the local default, or their opening
commit if on it or unavailable.

## Stacked workspaces

A stacked change is a chain of workspaces, each on a branch built on the one
below. A `--base` naming another workspace's branch, locally or on a remote,
records that workspace as the new workspace's base workspace.

```sh
shoal branch base                        # Show the current workspace's base workspace
shoal branch base set feature-a          # Record it by workspace number, ID or branch
shoal branch base clear                  # Remove it
shoal branch base set feature-a --workspace 4
```

The base must be another workspace of the same repository and must not create
a cycle; setting the current base, or clearing none, fails. Scoped callers may set their own workspace's base. Removing a base
workspace moves the workspaces stacked on it to its own base workspace, or to
none. `status` shows both directions; `list --json` and `status --json` give
`base_workspace` (`id`, `name`, `branch`, or null) and `stacked_workspaces`.

Once every watched PR of a base workspace has merged, with its HEAD among the
merged commits, the daemon moves the
workspaces stacked on it to its own base workspace and retargets their linked
PRs that still target the head branch of one of its merged PRs, which need not
be its own branch, to the branch it merged into. Their next
`watch` returns a `base_merged` update with the rebase command; the agent
rebases and force-pushes its branch. The PR is retargeted through the forge's
API with your [token](issues.md#forges) for that host, passed to curl on stdin. A
failed request is reported in the update.
