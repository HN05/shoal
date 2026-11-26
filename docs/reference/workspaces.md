# Workspaces

This file says how `shoal start` opens workspaces from links and branches, how
commands select a space, and what `list`, `inbox`, `status`, `rename`,
`branch rename`, `adopt` and `branch base` do.

```sh
shoal start my-project --branch fix-login
shoal start my-project --branch fix-api -a codex -- "Fix the API timeout"
shoal start https://forge.example/team/repo/issues/34 -- --model fast
shoal start --ci 2765                    # Start where a CI run ran
shoal list                               # Spaces, children below their parent
shoal inbox                              # What needs you
shoal status 3                           # One space: state, changes, links, sessions
shoal status pr 12                       # The spaces that link PR 12
shoal status resource devices            # The workspaces that hold a devices lease
shoal rename 3 "Login retries"           # Name the space in lists and tabs
shoal rename --request 3                 # Ask its agent to choose the name
shoal branch rename 3 fix/login          # Rename its branch; its name stays
shoal adopt ../existing-worktree         # Take ownership of a linked worktree
shoal branch base set feature-a          # Record the workspace this one builds on
```

## Start from a link

`shoal start <link>` takes an issue, PR, CI run or branch URL. The link selects
the one registered repository with the same origin. `-r`/`--repo` selects one
explicitly and must match the link. For a remote that is not registered, a
terminal offers to register it; otherwise the lookup fails and names the
`shoal repo add` command to run. Lookups use your
[forge login](issues.md#forges). Every start starts an agent, as
[Agents](agents.md) says, and arguments after `--` go to the agent. Setup,
hooks and the ownership checks of an existing branch apply to every form.

### Issues

An issue link, `shoal start <number>`, or `--issue`/`-i` with a number or URL
opens an open issue's workspace. A number names an issue in the repository
`--repo` names, else that of the current registered checkout or workspace, else
one a terminal picks. `--issue` also takes `--branch`.

The workspace already associated with the issue reopens. Otherwise the start
opens the issue's branch as an existing `--branch` opens, or creates it. The
branch is `<number>-<words>`: up to three title words, without filler such as
"a" and "the", within 32 characters. A title without ASCII words gives
`issue-<number>`. A failed lookup or ownership check creates nothing.

The agent's first message names the issue's number, title and URL, gives its
description when it has one, and points to `shoal view` for comments and later
edits.

### Pull requests

A PR link, or `--pr` with a PR's number or URL, opens the PR's head branch against
the PR's base, freshly fetched from the remote. Without a number, a
terminal picks among the open PRs, those carrying the `label` of the mode
`--mode` names where it has one. The start reuses a workspace that owns the
branch, and links the PR once the workspace is ready. A refused link only warns.

A PR from a fork has no branch in the repository. It opens in a
[workspace at a commit](create.md#workspaces-at-a-commit) at its head, below the space running
`shoal start`, or else top-level. The workspace takes the PR's title as its
name and `pr-<number>` as its directory. Shoal fetches the head from the ref
the forge serves it under, `refs/pull/<number>/head`, or
`refs/merge-requests/<number>/head` on GitLab, and refuses the PR on a forge
without one. Setup and the diff, `{diff_base}` included, compare against the
PR's `origin/<base>`. The workspace's agent has no branch, so it cannot push or
open a PR.

A PR has at most one workspace: a start reuses the one made for it or linked to
it. When the PR's head moved, the workspace moves to the new head; a workspace
with commits or changes of its own refuses that. An agent reuses the PR's
workspace only when it is below its own space.

### CI runs

A CI run link, or `--ci` with a run's URL or the number in it, starts where the
run ran, whatever its state. A number names a run in the repository `--repo`
names, else the current one. Without a run, a terminal picks among the newest
run of each workflow and ref whose latest attempt failed.

- A run for a PR opens as that PR's link does.
- A run on a branch other than the default branch opens that branch as
  `--branch` does.
- A run on the default branch, a tag or no branch names no work, so the start
  begins as a chat in its mode.

A mode that starts a chat starts one on any run. The mode is `--mode`'s, else
the default; a PR's labels choose none. A run refuses `--issue`, `--pr`,
`--branch`, `--copy`, `--in` and a second link.

The new or reopened space links the run as related, so its watch reports later
attempts. Its agent starts on the run: its workflow, number, ref, URL, commit,
state and failed jobs, with a pointer to `shoal view ci` for their logs. A chat
on a run is named `<workflow> run <number> on <ref>` unless `--name` names it.
A scoped caller's run of another repository is refused as
[`shoal link ci`](links.md#select-items) refuses it. `shoal create` takes a run of a PR or
branch the same way.

### Branch links

A branch link opens the origin branch, keeping its slashes and decoding URL
escapes. Shoal reads GitHub's `/tree/<branch>`, GitLab's `/-/tree/<branch>`,
Bitbucket's `/branch/<branch>` and Forgejo's `/src/branch/<branch>` routes.

## Branches

`shoal start <repository> --branch <name>` opens the branch as `git switch`
does. An existing local branch, or a remote one named `remote/branch`, opens.
Any other name creates a new branch of exactly that name. A name that would
collide with another branch is refused, never suffixed. Full `refs/heads/...`
and `refs/remotes/...` selectors tell names apart.

A branch that already has a workspace takes the start into that workspace, as
[`--in`](agents.md) does, and refuses what only a new workspace takes. That
workspace reopens without setup, hooks or refresh. A branch checked out
anywhere else, including the registered checkout's `main`, is refused.

A start that names no branch, issue, PR or copy begins as a [chat](chats.md) in
its mode, `worker` by default, under the chat's default name. This includes the
picker's "Create a new branch" and a start without a terminal. The mode tells
its agent to make the chat a workspace with
[`shoal convert workspace <branch>`](chats.md#convert-a-chat-or-a-workspace)
once it knows the work, before it changes any files. Flags that only a new
branch takes, such as `--base`, are refused there.

On a terminal, `shoal start` offers repositories, most recently used first. It
then asks for a new branch or picks among existing branches, open issues and
open PRs; a chosen issue or PR opens as its link would. The branch picker asks
the remotes for their current branches. A local branch wins over a remote one
of the same name and stays as it is. A remote branch is fetched into a new
tracking branch, or fast-forwards a matching local tracking branch without
dropping commits only it has. An ambiguous remote or an unrelated local branch
fails.

### Base branch

A new branch starts from the repository's default branch: `origin/HEAD`, or
without `origin` the only remote's HEAD. Several remotes without `origin` are
ambiguous. When the remote has no symbolic HEAD, Shoal asks it with
`ls-remote` and caches the answer; `git remote set-head origin --auto` updates
it. Without remotes, the registered checkout's current branch is the default,
and a detached checkout needs `--base`.

A new branch starts from the local default branch as the [sync](repo.md#default-branch-sync) left
it; creating it never fetches. `--base REF` starts it from any commit that
resolves locally, first fetching a remote branch it names. Shoal records the
resolved base for `shoal diff`.

A workspace on an existing branch diffs against the local default branch, or
against its opening commit when it is on that branch or it is unavailable.
`--base` sets the ref `diff` and `{diff_base}` compare against instead, for a
new worktree only.

### Suffixes

Shoal suffixes only the branch names it derives, such as an issue's or a
copy's; a name you type is refused instead, as [Branches](#branches) says. A
derived name that a local branch, a known remote branch, a branch namespace or
a retained Shoal record takes gets `-2`, `-3` and so on on its last
component. When a shorter branch blocks it, that component takes the
suffix: `feature` makes `feature/topic` into `feature-2/topic`. Other
components stay literal. A suffix never changes the space's name.

Names Git reads as a revision are reserved: `HEAD`, `@`, and hexadecimal names
of exactly 40 or 64 characters, a full object ID in either object format.
A derived name takes a suffix for them. A typed name, and opening an existing
branch of such a name, is refused.

### Worktree directories

A workspace's directory is named from its branch. Every character other than
an ASCII letter, a digit, `-` or `_` becomes `-`. Leading `-` and `_` are
dropped, the name is cut to 64 characters, and an empty one becomes
`workspace`. It takes `-2`, `-3` and so on while another workspace of any
repository has a directory of that name. The directory never moves.

Directories default to the repository's Shoal directory. `start --path <dir>`
puts this workspace in an exact new directory, relative to the current
directory, with `~/` allowed. An existing path, or one that overlaps Shoal's
state, a checkout, a workspace or another repository's directory, is refused.
Reopening a workspace accepts its current path but cannot move it. A custom
location has the same setup, ownership and cleanup rules as the default one.

## Space names

A space's name is the label that lists, tabs and notices show. It is one line
of 1–40 printable characters. Shoal trims whitespace around it and collapses
inner runs to one space. It refuses control characters, bidirectional
overrides, and a longer name someone sets; a name Shoal derives is cut to 40
characters ending with `…`. A name never selects a space and names no file,
directory, variable, session or branch, so names may repeat and renaming one
moves nothing.

A new space takes the first of these names:

- `--name`.
- For a CI job's workspace, the job's name.
- The title of the issue or PR it is started on or linked to.
- For a workspace, its branch as given, or without one the base and the short
  commit, such as `main at 84d4405`.
- For a chat, `<repo> <mode>`; in a workspace's worktree,
  `<workspace name> chat`; for a master session, its mode, such as
  `orchestrate`.

Only `shoal rename` changes it later. A branch rename leaves it as it is, and
`shoal restore` gives a workspace back the name it had.

### Rename a space

`shoal rename [SPACE] NAME` gives a workspace or chat a new name. Its agent
sets one to say what the work is. Without a space, it renames the current
workspace, else the chat it runs in, else the current checkout's only running
chat, else one a terminal picks. Scoped callers rename their own space and the
spaces below it.

The tabs, windows and blocks Shoal opened for the space, and those its agents
run in, take the new name wherever the command runs, while they still show the
label Shoal gave them. One the user named keeps its label. The worktree
directory, branch and zmx session keep their names.

`shoal rename --request [SPACE]` sends the space's agent an
[alert](alerts.md) asking it to choose a name and set it. It is refused while no
agent runs there. `shoal rename` without arguments picks a workspace or running
chat and asks for its name.

## Space numbers

Every space, a workspace or a [chat](chats.md), has a number that no other
space holds on any machine joined to the same [hub](hub.md). It is the smallest
free number when the space is created. It stays through renames and stops, and
a later space may take it once the space is removed. `shoal status` shows it,
and `--json` gives it as `number`.

A removed space has no number, and its ID selects it. While its log keeps the
removal, a command that needs the space to exist is refused, saying it was
removed, and naming the repository when removing the repository took it.

## Select a space

Every argument that takes a space selects it by its number or ID, such as
`shoal attach 3` or `--space 3`, or by `-` for
[the last space](#the-last-space). Other text is refused with "a space is
selected by its number or ID". `--add-blocker` is the one exception, where a
bare number names an issue.

A command that acts on one space takes it as an argument, or with
`-w`/`--workspace`, or `-s`/`--space` where it also acts on chats. Without one,
it acts on the caller's scoped space or the workspace that holds the current
directory, then opens a picker on a terminal. A deleted current directory
names no workspace. An invalid target or other invalid argument fails without
a fallback, before any prompt for the omitted ones. Without a current
workspace, noninteractive and [JSON](output.md) calls need a target.

Scoped callers stay within their own space and the spaces below it, such as
[child workspaces](create.md) and those a chat below opened. Without a target,
a scoped command acts on the workspace below whose directory it runs in, else
on the caller's own space. Agent hooks always report for the caller's own
space. Listings of every workspace, and `--all`, keep that scope.

### Pickers

A picker shows the spaces' names. It leaves out spaces without what the command
acts on, such as an agent session for `attach` or a running agent or command
for `stop`, and says so when none is left. Spaces the command would refuse,
for their state or for running `setup` work, come last with the reason in place
of their status. The daemon of the machine each space runs on judges it from
the request the command sends for it.

A command that accepts `-A`/`--all` offers "All workspaces" as its picker's
last entry and its workspace argument's first completion. Another omitted
required argument that names an existing record opens a picker of those
records, under the same noninteractive rule; the refusal names the missing
argument and how to pass it. `config show` checks the registered checkout
before the picker, as
[Inspect effective configuration](config.md#inspect-effective-configuration)
describes.

Leaving a picker without a choice exits with status 130 and no message. A
command whose output reader closes early, such as `head`, exits with status 141
and no message.

### The last space

`cd`, `attach` and the commands that open, show or run something in one space
remember the spaces each acted on, across terminals, workspaces and chats
alike. They take `-` for the latest one other than the current workspace and
the space the command runs in. `cd` also remembers the space it leaves.
`attach` remembers the space of each agent session that it, or a launch such
as `start` or `claude`, showed here and left running, also on another joined
machine, and of each place a focused `start` opened.

`-` passes over removed spaces, and `attach -` over chats that no longer run.
A command that acts only on workspaces keeps a chat named to it out of its
history. Scoped callers keep no such history.

## List

Bare `shoal` on a terminal opens a picker on the [inbox](#inbox) when
something needs you, searching for the current workspace's items when it has
any, and otherwise on the list. `shoal inbox` and `shoal list` open it on
their own view. Tab cycles through the inbox, the list and recent alerts. Showing the alerts marks them read, as `shoal alert list` does, and
Enter on one enters its workspace. Workspace processes see only the list.
Without a terminal, bare `shoal` prints the commands grouped by task and a
starting workflow, as `shoal --help` does. Commands that only Shoal, its agent
hooks and integrations run live under the hidden `shoal internal`.

`shoal list` with `-p`/`--print`, a filter, `--json` or output that is not a
terminal prints the list instead. It lists workspaces and chats, each chat
with its agent, and each space followed by the spaces below it. It also lists
each agent waiting to start in a workspace the list has no row for, such as a
new one, with its place in the queue and what it starts.

- `--chats` lists only the chats.
- `--repo <repository>` lists only that registered repository's workspaces,
  chats and queued agents.
- `--status <status>`, repeated or comma-separated, lists only the rows with
  one of those statuses, spelled in kebab case, such as `needs-input`,
  `process-running` or `queued`.

`shoal list` and its picker order spaces by status, then number: working
agents and processes first, then what needs you, failures, changes under way,
queued agents, and quiet spaces last. Branches and paths are in
[`shoal status`](#status).

### Rows

The rows of the list, the inbox and space pickers are aligned. Each starts with
a mark of what its status asks of you:

- ● your turn to answer, review or resume;
- ◐ an agent or process works;
- ○ quiet;
- ◌ changing state;
- ✗ failed.

A row then shows the space's number, its name, its repository when the rows
span several, its status, what its processes and containers use while any run,
and the linked issue's title or the CI job. It is cut with `…` to fit the
terminal.

Usage counts every process that carries the workspace's `SHOAL_SPACE`, with
the children those processes have waited for. CPU is a percent of one core
since the daemon's previous sample, taken every ten seconds and when a
workspace's containers start. Memory is resident memory on Linux and the
physical footprint on macOS. Both include what the workspace's
[containers](resources.md#containers) use. `shoal status` adds the process
count, and `--json` gives `usage` in bytes and percent.

### Statuses

A workspace's status is its lifecycle state while it is not ready. A ready
workspace shows the first of these that holds:

- `queued (N)` while nothing Shoal started runs there and an agent, other than
  one asleep there, waits at place N in the [agent queue](queue.md) to start in
  it, or `starting` once the queue admits it. Each is followed by what else it
  waits for, as `shoal queue` shows. Another status names such agents after
  it, as in `process running, queued (2)`.
- `stopped` while stopped agents or commands wait for `shoal resume`, and the
  workspace is neither done nor [blocked](progress.md).
- `asleep` while its agent is [asleep](sleep.md).
- `needs input`.
- `blocked` while its progress records what blocks it.
- `finished` after `shoal done`, also while its work is stopped, until
  `shoal done --undo` or `shoal resume` withdraws the completion.
- `working`, `ready` and the other [agent states](agents.md#agent-state).
- `process running` while a command, or an agent that reports no state, runs.
- `parent working` while no agent of its own has ever started there and the
  agent of the nearest space above with one works, as when that agent's
  subagents work in a child it created.
- `inactive`.

A chat's status is `asleep` while its agent is asleep. A stopped chat shows
`stopped` until it is restored, or `finished` or `blocked` when it is.
Otherwise it is the first of `needs input`, `blocked`,
`finished`, `working`, `ready`, its agent's other states, and
`process running` before its agent reports one.

### Keys

The header names the keys the list binds, most used first, on as many lines as
the terminal needs. Each action returns to your shell.

- Enter enters the selection.
- Ctrl-T attaches.
- Ctrl-F shows the diff.
- Ctrl-V lists the workspace's linked issues and PRs in its repository, where
  Enter shows one with `shoal view` and Ctrl-O opens it in the browser with
  `shoal view --web`.
- Ctrl-A adds a space.
- Ctrl-E runs Claude or Codex CLI, starts a Happy session, opens the Codex or
  T3 app, or runs a shell command.
- Ctrl-O shows status.
- Ctrl-S stops.
- Ctrl-D deletes.

## Inbox

The inbox lists what needs you, each item with its workspace:

- threads whose turn is yours, with their [number](threads.md#numbers);
- pending approval requests that no coordinating agent decides, including
  issue [proposals](approvals.md#approval-for-agent-issues-and-prs) whose
  workspace is gone;
- workspaces and running chats whose agent needs input;
- blocked workspaces and chats, with the first of what blocks them;
- failed workspaces;
- workspaces and chats whose agent is [asleep](sleep.md), with why, what wakes
  it and the commands that wake it now;
- the [ready marks](completion.md#ready-for-review) of spaces whose agent is
  not working, one row per marked item;
- running [demos](demos.md);
- Git worktrees under a repository's Shoal directory that no workspace owns;
- automatic cleanup that fails other than in a workspace;
- repositories whose default branch has failed to [sync](repo.md#default-branch-sync) for 30
  minutes.

Enter acts on the item:

- a thread: shows it and reads your reply;
- a request: asks whether to approve or deny it, and refuses one that has
  replaced the request listed;
- an agent that needs input or is asleep: attaches to it;
- a blocked space: opens its actions;
- a failed workspace: shows its status;
- a ready PR or issue: shows it with `shoal view`, or `shoal status` for a mark
  on the workspace itself;
- a demo: opens it;
- an untracked worktree: adopts it with
  [`shoal adopt`](#adopting-a-worktree);
- failing cleanup: shows Shoal's status, as `shoal status` outside a space
  does;
- a failed sync: retries it with `shoal repo sync`.

Ctrl-T, Ctrl-O, Ctrl-F and Ctrl-V act on the row's workspace, and Ctrl-A adds
one.

`shoal inbox` with `-p`/`--print`, `--repo <repository>`, `--json` or output
that is not a terminal prints the inbox. `--json` gives each item's `kind`:
`thread`, `approval`, `needs_input`, `failed`, `asleep`, `ready`, `demo`,
`untracked`, `cleanup` or `sync_failed`. The item carries its `thread`,
`request`, `workspace`, `demo`, `worktree` (`repository_id` and `path`),
`error` or `sync`, and a ready item its `mark`. A workspace process gets only
its own items, printed, and never untracked worktrees, failing cleanup or
failing syncs. After clearing its scope token it gets none.

## Status

`shoal status [SPACE]` summarizes one space, a workspace or a
[chat](chats.md), running or stopped: the one named, else the current one.

Status shows:

- the space's state;
- for a workspace, its changes since its fork point and what it holds;
- its agent sessions, each with the path of its transcript on this machine;
- whether each assigned issue is open or closed;
- each watched PR as the forge reports it now: open, merged or closed, merge
  conflicts, each CI check's result, and a review state from each reviewer's
  latest approval or change request. A check is `waiting` while no runner has
  taken its job, where the forge tells that apart. The CI runs on an open PR's
  head that [wait for approval](ci.md) are in `waiting_runs`;
- the opened and related items;
- the [threads](threads.md) the space opened that wait for their holder, in
  `waiting_threads`;
- the approval requests that wait for a decision, the space's own and those it
  decides, in `pending_approvals`;
- its [attached files](files.md) in `attached_files`, and its unread alerts in
  `unread_notifications`;
- a failed step of automatic cleanup in the workspace, in `cleanup_error`;
- `Last sync`: when the repository's default branch last [synced](repo.md#default-branch-sync), and
  why the syncs since then failed, in `sync` (`synced_at`, `failing_since` and
  `error`);
- the files the last check of HEAD against the default target found
  [changed on both sides](branches.md#files-changed-on-both-sides), from a watch or
  `shoal conflicts`, in `changed_on_both_sides` (`target`, `target_commit`,
  `head` and `files`);
- the first condition that keeps the space from automatic cleanup, in
  `cleanup_reason`;
- the chats in its worktree, in `chats`.

A failed lookup reports its error on its item without failing the command.
The conditions that keep a space from cleanup come in this order: holds,
threads it opened that wait, a pending PR proposal or push request, a running
chat in it, queued agent launches, the CI runs of a merge it follows, and
assignment state, then commands, leases and Git state. When nothing keeps it,
`cleanup_reason` names the idle delay.

A chat's status leaves out what needs a worktree: its branch, changes, setup,
executions, leases and sync. It names the workspace the chat runs in as
`in <workspace>`, which `shoal list --chats`, the inbox and the menu show too.

### Status as JSON

`--json` returns the same data. It gives `kind`, `workspace` or `chat`, with
the space's record in `workspace` or `chat`, PRs in `prs` and the issue in
`issue_status`. It gives sessions in `sessions`, or in `sessions_error` a
failed `zmx list` or a session zmx cannot reach.

`transcripts` lists every session the space's agents reported, with:

- `agent` and `session`;
- `execution`, the execution a workspace's agent ran as;
- `current`, whether the agent runs it now;
- `path`, its transcript;
- `files`, all its files, the transcript included, which Shoal finds where the
  agent's tool keeps them and never reads;
- `continued_from`, for a session a
  [handoff](chats.md#convert-a-chat-or-a-workspace) started, the `agent`,
  `session` and `integration` it took over from.

A session whose files are not on this machine, or of an agent whose tool Shoal
does not know, has no `path` and no files. One whose files could not be looked
for says why in `error`. Sessions the agents' hooks never reported, such as
those before a `/clear`, are not listed.

The space records of `list --json` and `status --json` name their `kind`. Their
`workdir` says where the space's agent works, with its `path` and its own
`kind`:

- `worktree`, with the `workspace` that owns it, for a workspace and for a chat
  in a workspace's worktree;
- `checkout`, for a chat in its repository's registered checkout;
- `root_dir`, for a chat without a repository.

Their `links` hold the assigned issues in `issues`, first assigned first, the
assigned PRs in `prs`, and the items recorded as opened or related in `opened`
and `related`. Each item has `kind`, `url` and `title` when known. An
acknowledgement without a PR URL leaves `links.prs` empty.

### Find the spaces of an item

`shoal status` with an issue, PR or CI run URL, or with `pr`, `issue` or `ci`
and a number or URL, lists every space that links the item, as a JSON array. A
number must be linked in only one repository. `resource` with a pool or member
name lists every workspace that holds it. Scoped callers find only their own
workspace and its child workspaces.

## Rename a branch

`shoal branch rename [WORKSPACE] BRANCH` renames the branch a ready workspace
has checked out. The workspace keeps its name, worktree path, `{workspace}`,
and the resources and settings it owns. Its uncommitted files stay. Git keeps
the branch's upstream and reflog. Remotes and the head branches of open PRs
stay as they are, as do PR watches and acknowledgements.

The new branch must be free locally and in every other workspace of the
repository; the rename picks no suffix. It refuses the default branch, a
branch checked out elsewhere and a reserved name. A scoped caller renames its
own workspace's branch while no other execution is recorded there. An unscoped
rename needs no recorded executions. The caller keeps its environment, and
new commands get the renamed identity.

An interrupted rename leaves the workspace failed until `shoal branch rename`
runs again. That run first finishes the interrupted rename when Git completed
it, or cancels it when the branch is unchanged, then renames to the branch it
is given. Normal cleanup still removes such a workspace once its worktree is
confirmed deleted.

## Adopting a worktree

`shoal adopt <path>` registers an existing linked worktree in place and enters
it with shell integration. The path is relative to the current directory, with
`~/` allowed, and names the worktree's root. The repository is the one
`--repo` names, else the one whose checkout owns the worktree, else the only
one that shares its `origin` remote, else one a terminal picks. The workspace
is named after its local branch.

Adoption refuses an ownership conflict, a main checkout, a detached or locked
worktree, and a path that overlaps a protected location. Adopting a verified
ready workspace again reopens it. Adoption cannot repair a moved managed
worktree.

At a workspace's recorded path, adoption reclaims a worktree that exists but
that Shoal cannot verify it owns. Check the worktree yourself first: adopting
re-marks a linked worktree of the recorded repository on the recorded branch
that no other workspace owns, then restores it to ready. A failed setup is no
ownership failure, and adoption refuses it.

Adoption keeps commits, uncommitted files and Git settings, and runs no fetch,
setup or post-setup hook. Shoal takes full ownership, with normal cleanup that
includes the pre-remove hook. To keep adopted work indefinitely, turn off
automatic cleanup in the repository's config first. Run `shoal setup` when you
want setup.

`--copy` creates a new branch worktree in the repository's Shoal directory,
copies the source worktree's files into it, and leaves the source in place.
Shoal adopts the copy without setup. Its branch takes the next free suffix when
the source branch is checked out.

## Stacked workspaces

A stacked change is a chain of workspaces, each on a branch built on the one
below. A `--base` that names another workspace's branch, locally or on a
remote, records that workspace as the new workspace's base workspace.

```sh
shoal branch base                        # Show the current workspace's base workspace
shoal branch base set feature-a          # Record it by workspace number, ID or branch
shoal branch base clear                  # Remove it
shoal branch base set feature-a --workspace 4
```

The base must be another workspace of the same repository and must not make a
cycle. Setting the current base, or clearing none, fails. Scoped callers set
their own workspace's base. `status` shows both directions. `list --json` and
`status --json` give `base_workspace` (`id`, `name` and `branch`, or null) and
`stacked_workspaces`.

Removing a base workspace moves the workspaces stacked on it to its own base
workspace, or to none. So does the merge of every watched PR of a base
workspace, with its HEAD among the merged commits. Then the daemon also
retargets the stacked workspaces' linked PRs that still target the head branch
of one of its merged PRs, which need not be its own branch, to the branch that
PR merged into. Their next `watch` returns a `base_merged` update with the
rebase command, and the agent rebases and force-pushes its branch. Shoal
retargets the PR through the forge's API with your
[token](issues.md#forges) for that host, passed to curl on stdin. The update
reports a failed request.
