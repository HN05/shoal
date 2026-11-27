# Spaces

This file covers `shoal-spaces`' workspaces and the tree of spaces: launches
and creation, the tree, stacking, adoption, workspaces at a commit, copies,
refresh, conversion and numbers. [Workspaces](../reference/workspaces.md),
[Child workspaces](../reference/create.md),
[Agent queue](../reference/queue.md#launch-a-new-workspace) and
[Convert a chat or a workspace](../reference/chats.md#convert-a-chat-or-a-workspace)
say what each command does; this file gives the reasons.

## Spaces

### One name for both kinds

The user reads a workspace and a chat by their names and selects both by
number or ID. What applies to both is therefore named for the space, and the
flag that selects one is `--space`.

### Numbers

Each space has a number: the smallest that no other space holds when it is
recorded. One sequence covers every repository and every machine of a hub, as
[Space numbers](hub.md#space-numbers) says. The number thus stays short to
type and read.

The number lasts as long as the space. It survives renames and stops, and the
space's removal frees it.

Every argument that takes a space selects it by number or ID. One resolver in
the daemon and one in the client read them. The client refuses text that is
neither before it asks the daemon.

A name is a label people read. It never selects a space, so names may repeat.

A number selects only a space that exists. A removed workspace and a queued
launch are selected by ID.

`--add-blocker` is the exception, since it names blockers: a bare number there
is an issue, as `shoal start` takes one.

## The tree of spaces

### One parent link

Spaces form one tree through one parent link, `space_parents`. It records a
workspace created below another and a chat opened below one alike.

Every space has at most one parent, never a space below it. The parent is a
ready workspace or a chat of the space's own repository, or a chat without a
repository. Shoal refuses a child in another repository, so spaces of several
repositories share a parent only in a chat without one.

Every space has one row in `spaces`, with its kind and what every space has.
Its workspace or chat row extends it with what only its kind has. A conversion
keeps the `spaces` row. Deleting it removes the space with what each table of
spaces holds for it.

### Walks

Every reader of the tree takes one walk up or one walk down the parent link,
with no branch on kind. The store offers both walks, as
[Composition](daemon.md#composition) says.

A walk asks each space one question: whether it has an agent. A space has one
when its workspace's agent runs, its chat's agent runs, or a restore of either
waits in the queue.

### Scope through the tree

A scope reaches its space and every space below it. A chat without a
repository reaches every space. One operation names the spaces a scope reaches,
for every check.

The CLI resolves a scoped command, a workspace's or a chat's, to the workspace
below whose directory it runs in, else to its own space. A chat's agent that
changes into a workspace it opened thus acts on that workspace, as a
workspace's agent acts on its child. Agent hooks always report for the agent's
own space.

[Scope](permissions.md#scope) says what a scope allows.

### Coordinators

A space's coordinator is the agent of the nearest space above it that has one,
whatever its kind. One operation queues a message for it.

The events that would notify the user about the space become the
coordinator's messages instead, as
[Coordinators](../reference/create.md#coordinators) lists. They fall back to
the user when the space has no coordinator or the coordinator's messages are
full.

Events that a process causes in a child without an agent of its own are the
coordinator's own work, so they reach the user.

The coordinator answers the threads opened below it, or forwards them up. It
may stop, restart, resume and convert the spaces below it and discard their
stopped work, since only it knows whether a child's agent still has work.

The coordinator resumes a stopped agent, whoever stopped it: the user, Shoal or
a crash. A workspace's agent goes through the queue, which starts it detached.
A chat's starts in a background place, as the coordinator starts chats.

The coordinator confirms, as the user does, that an execution Shoal cannot
prove ended did end. The check of a survivor's birth identity still keeps
unknown ownership from authorizing a kill.

Every such check asks whether the caller's scope reaches the target. A refusal
names the space outside it.

### Activity of the spaces below

The parent's agent, a workspace's or a chat's, receives the issue and PR
activity of the spaces below it that have no agent of their own, and of theirs
in turn. Each update names its space. It arrives through the same cursors,
deliveries and acknowledgements as the parent's own.

A child's own agent takes over its subtree. That is an agent that reports its
turn state, or one Shoal tracks there, as it tracks custom and Happy agents
that report none. A child whose restore is queued takes over too, since its
agent receives what arrived once it starts.

### Stops

Stopping a space stops the spaces below it, workspaces and chats, since their
agents work for it. `--skip-children` stops only the space.

### Approvals

Approval requests go up the tree: to the nearest space above with an agent, of
either kind, since each knows best what its children need. The user decides at
the top.

A parent space approves a request that a permission governs where its own
value for that permission is `direct`. It approves anything else only where it
holds an approved grant for it, and it approves a hold on its child. Otherwise
it denies the request or passes it up.

A parent whose `decide` permission is `direct` decides any request below as
the user would. It still approves a request that a permission governs only
where it holds that permission at `direct`.

A chat whose agent stops hands the user the requests it had not decided, since
it can decide no more. So does a deciding space that is removed.

A space lists the requests it decides beside its own, whatever its kind, since
it must find them to decide them.

### A child's lifetime

A child keeps its parent from merged cleanup only. Merged cleanup would
otherwise take a child's open work because the parent's own branch merged. The
parent's other cleanup follows its own rules.

A child never outlives its parent. Every removal of a parent, manual or
unattended, first removes the spaces below it, deepest first, whatever their
work, holds or threads. It takes the shared [removal path](cleanup.md#removal),
which keeps a checkpoint and the branch of each workspace but a disposable
one. A child that cannot go retains the parent. The journal and the removal's
output name the children it took.

While a parent's completion with cleanup stands at its HEAD, the spaces below
it go too after the grace period, whatever their own work. Their own
keep-reasons, or a space still below them, keep them, as
[A parent's completion](../reference/create.md#a-parents-completion) lists. A
child's own `done` follows the usual rules.

The parent owns its children, so its processes may remove and hold them. They
remove a space with the user's hold on it or below it only as their
`user_holds` permission allows, as [Creating and removing
spaces](permissions.md#creating-and-removing-spaces) says.

### Subagents in a child

A subagent may work in a child's directory instead of an agent of the child's
own. The child then has no agent of its own, so its activity reaches the
parent. The subagent reports to the parent directly, and Shoal need not relay
that supervision.

Shoal does not track those subagents. Their hooks report under the
coordinator's scope from its directory, so Shoal cannot tell which child a
subagent works in.

A workspace where no agent of its own has ever started is therefore its
coordinator's work. Its recorded mode tells whether one has. It lists as
`parent working` while the coordinator's agent works. Idle cleanup keeps it
while that agent runs, in any turn state, since subagents may outlast a turn
and a coordinator may wait for the user mid-delegation. A chat below the
coordinator whose agent stopped is its work the same way, so idle cleanup keeps
that chat too.

Claude Code's worktree hooks make the worktree it creates for a subagent such a
child. Its isolation thus needs no worktree that Shoal does not own.

A subagent inherits its agent's scope, and under Codex its instructions and
directory. A `SubagentStart` hook therefore names the workspace the subagent
works in. Without that hook, a Codex subagent finishes its agent's assignment,
as the instructions say. [Subagents in a
child](../reference/create.md#subagents-in-a-child) says what the hooks do.

## Launches

### One request for every start

Every start goes through one request, `Launch`, whoever makes it. It starts a
new space or an existing workspace's agent. An app thus needs no configuration
of its own to start an agent as the CLI does.

A launch carries what `shoal start` takes, as its command line gave it. Another
command's start adds what only it has, such as the subject a repository's setup
gives its agent and the setup mode, which no other launch starts in. The daemon
records a client's launch as `manual`, and names the source of a launch it
makes itself. The agent's queue entry carries the source to the agent's start,
and the journal records it there.

The daemon alone decides what the launch leaves out, with the steps in
`shoal-core`:

- the mode;
- the kind of space;
- the repository, from what the launch names, from what its issue or link
  belongs to, or from the checkout or worktree it was made in;
- the branch, from a PR, a branch link, the typed name or earlier work on its
  issue.

The launch then queues the agent of the workspace `--in` names, or of the
workspace that has the branch. `QueueAgent` queues an existing workspace's
agent the same way, on the workspace's issue. Otherwise the launch starts a new
space. A launch without a branch or issue starts a chat.

Every launch answers with the space it started in, as it created or found it,
and how it started the agent there. A caller thus reads the space without
asking its kind. A background place's worker reports only where the place runs.

### One sequence for every new space

`shoal start`, `create` and apps start a new space through one sequence,
whatever its kind. `StartWorkspace`, `StartCreation` and `StartChild` take it,
and so does a launch's new space. The sequence:

1. refuses an agent that cannot run, an issue that is not open, or a branch
   that is not a literal Git branch name, before it creates anything;
2. names the space as the launch names it, else after its issue's or PR's
   title, else after where a chat runs;
3. records the space, or reopens the workspace on the branch, PR head or record
   it names;
4. assigns the issue, removing a new space it cannot assign;
5. queues the agent the request asks for through the queue's one request, with
   its mode, model, arguments, fork, issue and the caller's place;
6. starts a workspace's preparation.

The request names an agent, mode or model only where the caller chose one. The
daemon resolves the rest under the space's settings, `agent.default` for the
agent. The agent works on the start's issue unless the request gives it an
issue or a task.

The kind decides only where the space goes: a chat goes in a checkout, a
workspace's worktree or `root_dir`, and a workspace gets a new worktree. The
daemon reads the issue the same way for both. Preparing a workspace is the only
step that depends on the kind.

The daemon records every new chat before its agent runs anywhere, below where
it goes and linked to what it starts on. The caller's terminal, a background
place, the queue or an app only runs the recorded chat's agent. A start that
names nothing to link thus sends no link request.

### Every start starts an agent

One rule covers a new workspace, a new chat, `--in` and `contribute`, whatever
else the start names. [Start an agent](../reference/agents.md#start-an-agent)
says which agent starts. The daemon applies the default and the refusal, so a
start from any client follows the rule. `shoal create` makes a workspace
without an agent.

`create` is thus the one way to a workspace without an agent: at the top from
the user's terminal, or as a child inside a space. An agent whose work needs
another PR gets another branch and worktree instead of switching branches or
creating worktrees Shoal does not own. The daemon places and records it as a
launch's new workspace, with nothing to queue.

### Fresh or forked agents

`shoal start --fork` gives a child the parent's agent from a fork of its
session, so the parent need not restate what it learned. [Fork the parent's
agent](../reference/create.md#fork-the-parents-agent) says which session it
forks.

A fork of the same model reads the parent's prompt cache. It still re-reads the
whole context on each of the child's requests, and fills its context window
sooner. `start` therefore keeps a fresh agent as its default. The shipped
`work` rule tells agents to fork for a short task that needs their context, and
to start fresh for long work or a task that most of it does not concern.

The new agent holds its own space's scope, never the caller's.

### What the CLI keeps

The CLI parses the start and picks on a terminal what a picker picks there. It
adds what only its process knows, such as its directory, its environment and
the session `--fork` forks. It adds what only its terminal can check, such as a
host to place the agent in.

The CLI keeps what attaches a terminal: a start here, a focused start that
opens its place beside the terminal and shows the preparation there, and an
agent that runs its own way, such as Happy's. Happy's agent starts in the
caller's process for a chat as for a workspace.

Such a launch says so with `here`. The daemon queues the agent as for any
start, and keeps its entry for the caller. The caller holds the entry while it
follows the preparation, then claims it. The claim starts the agent in the
caller at once where the start asked for that, else when the limits admit it.
Otherwise it leaves the agent to the queue's background start.

Any other start reports the recorded space and its agent's entry without
following the preparation, unless `--wait` asks it to.

One `shoal_client` call sends the request and follows the preparation. It
returns the prepared workspace, or the typed failure with the workspace as the
failure left it: ready when only what follows setup failed. The CLI makes that
call, and keeps its prompt for a failed setup and an agent it runs in the
terminal.

An interrupt or termination of the CLI cancels the preparation, which
interrupts the worker as it interrupted the CLI. A CLI that ends otherwise
leaves the preparation running.

### Places

Every agent start has a queue entry, admitted at once where no limit applies.
The queue starts every admitted agent the same way, opening a place for any
recorded space. The CLI runs a recorded space's agent through one placement
step for both kinds: here, beside, in a background place or detached. A
workspace's agent starts once its preparation ends.

One place worker runs any recorded space's agent in a place, in the space the
daemon opened for it. No other process reopens a workspace or links what its
start names. One builder makes the agent's command from the space's records.
Only what the record holds differs by kind, such as a chat's home and the
instructions its agent receives first.

A chat an agent starts opens in a background place, which the queue opens once
it admits the chat's agent. The agent cannot hand over its terminal.
`start --here` instead runs the chat in the caller's own terminal. The start
returns once the daemon has recorded the chat and queued its agent.

A chat run in the caller's terminal is the chat's own execution, as a
workspace's agent is. The caller's stops leave it running. Places drop the
marks and scope token of the space that opened them, so what runs there is the
new chat's.

### Warnings and failed starts

What linking a new space warns of, of either kind, goes to its coordinator,
else to the user's alerts. It never goes only to a terminal that the agent then
takes over.

A start that fails fails its queue entry. A space whose place never starts its
agent stays, whatever its kind, with its launch as a `failed` entry. Nothing
admits that entry, and it keeps no space from cleanup. The failure reaches the
starter the same way as a warning, naming `shoal resume <space>`, which queues
the launch again as it was, and `shoal rm <space>`. A new launch in the space
replaces the entry, and the space's removal drops it.

### Scoped starts

A scoped caller queues agents only for the workspaces its scope reaches, but
not its own. [The spaces below](permissions.md#the-spaces-below) lists what
else it may do there.

A scoped caller's start that a blocker holds records its workspace below the
caller at once, as any start does. The waiting preparation records the
caller's space, in whose scope it runs once the work is done.

## Preparing a workspace

### Recorded first

A start records a new workspace as `preparing`, without its worktree, then
starts its preparation. The preparation creates the worktree, sets it up,
links its PR and runs a new workspace's post-setup hook. Its agent waits in the
queue for the preparation.

The request answers once the workspace is recorded. A caller thus gets its
number and its agent's entry without waiting for Git or setup. Until the
worktree exists, what reads the workspace refuses it, and removal takes only
its record and leases.

A failed preparation reaches the agent of the nearest space above the
workspace with one, or else the user, as other news does that the caller does
not wait for. The message names `shoal setup`, which retries the preparation,
and `shoal rm`.

The preparation is an [operation](daemon.md#operations), which a client
follows by its ID.

### Preparations that wait

A preparation that creates a new worktree first waits for the blockers of the
agent's launch, in a record beside the workspace. It then creates the worktree
on the branch of the done blocker that holds the others' work, or from the
default branch when none leaves one. The workspace thus never starts behind
its blockers.

A start that nothing follows also waits while disk protection holds back new
starts.

The queue's dispatcher starts the preparation once that allows, in the scope of
the space that started it. A stopped agent leaves it waiting for an explicit
preparation.

### Opening for an admitted agent

Before any process runs an admitted launch's agent, the daemon opens its
workspace for it:

- The workspace must still be its ready, verified worktree.
- A branch that holds nothing of its own fast-forwards to what it builds on, as
  `shoal refresh` does, so the agent starts on a current branch.
- The daemon links the PR that the start names. A refused link warns the
  starter.

A copy is made when its preparation runs, which has waited for what the copy
waits for.

### Commands during a lifecycle operation

Some lifecycle operations return the workspace to ready or failed on their own,
such as the update an admitted agent's opening makes. A command, merge or
unmerge whose start falls in one waits for it to end. It is then prepared again
for the workspace the operation leaves. The window is Shoal's own, not a reason
the command cannot run.

The store counts each workspace's lifecycle changes. A start whose
registration fails after such an operation came and went since its first look
is prepared again too, since the worktree may have been unverifiable meanwhile.

Removal, a failure and setup still refuse the command. They may wait for other
work first. They refuse an agent's registration the same way. That
registration holds the queue admission that a stop of its workspace needs.

### One path for every worktree

Every other creation records the plan of its worktree with the workspace and
creates it through the same path, only at once. That covers a split, a
conversion, a restore, a copy, a workspace at a commit, a CI job's workspace
and `adopt --copy`. A
failure at any step thus leaves a workspace that `shoal setup` retries.

A restore whose worktree fails still opens the space with its links and agents,
and the workspace is failed. The retry thus needs nothing of the removal
record.

### Cut-short creations

A workspace's record says whether Git created its worktree. A pending one keeps
its plan and what an earlier try recorded. The store refuses a created one
without its base and admin directory.

Git creates a worktree before Shoal records its admin directory. A retry or
removal of a pending workspace without one therefore takes the admin directory
whose `gitdir` names its path, unless another workspace marked it. A pending
workspace without such a directory has no worktree.

An add that Git still marks as under way was cut short before its checkout
finished. A retry deletes it and adds the worktree again while its directory
holds only the `.git` link the add wrote. It refuses while the directory holds
files, which may be someone's work.

A retry that writes files into a worktree an earlier try left first keeps what
it holds in a checkpoint.

Only a created worktree is checked for a move when it goes missing. After a
`git worktree move` during a cut-short creation, `shoal rm` forgets the record
and keeps the branch and the moved files.

## Issues and links

### Issue workspaces

An issue's workspace needs an open issue. The start records the issue's
canonical URL before tracked setup or the agent's launch.

Reopening first follows the issue's association, then an existing local branch
with the derived name, rather than suffixing a new branch. A conflict fails
before the agent picker. Associations are idempotent and cannot be replaced.

The derived branch is the issue's number and at most three title words, short
enough to read in full in branch lists. The workspace takes the issue's title
as its name, which says what the work is.

### Links

`start <link>` opens the work the link names, as
[Start from a link](../reference/workspaces.md#start-from-a-link) says.

A fork's PR has no branch in the repository, so its head opens in a
[workspace at a commit](#workspaces-at-a-commit).

A CI run's start goes where the run ran. A run on the default branch, a tag or
no branch names no work, so its start begins as a chat. The run says nothing of
the mode, so a PR's labels choose none.

`--ci` without a run offers only the newest run of each workflow and ref, so a
failure that a later run fixed is not offered.

## Stacks

A stacked workspace records the workspace whose branch it builds on. Shoal
infers it when the creation base names another workspace's branch, locally or
on a remote. Stacks thus span workspaces on any forge.

The base is local metadata, not a link. Removing the base workspace moves its
stacked workspaces down to its own base, and journals the change.

The PR sweep does the same once every watched PR of the base merges and
covers its HEAD, before cleanup can remove it. It also retargets the stacked
workspaces' PRs, as [Stacked
workspaces](../reference/workspaces.md#stacked-workspaces) says. Retargeting
goes through the same REST transport as issue and PR actions.

Agents rebase their own branches. Shoal moves a workspace's branch only by a
fast-forward: the one `shoal refresh` makes when asked, or the one an admitted
agent's start makes.

## Adoption

Adoption takes ownership of an existing linked worktree in place. It keeps the
worktree's files and Git settings, and records the workspace ready without
setup or hooks. [Adopting a
worktree](../reference/workspaces.md#adopting-a-worktree) says which worktrees
it accepts. The workspace takes normal cleanup ownership.

Reopening an adopted workspace verifies its record. Adoption cannot repair a
moved managed worktree. It repairs one whose metadata was replaced only by
reclaiming it at its recorded path, as [Lost worktrees](cleanup.md#lost-worktrees)
says.

`adopt --copy` records a preparing workspace before it creates the copy's
worktree, so a failure keeps its plan, as [One path for every
worktree](#one-path-for-every-worktree) says. Its retry copies the source again,
and fails naming the source once it is gone. The copy also drops the checkout
files that the source deleted, at any depth.

## Workspaces at a commit

A workspace may sit at a commit instead of on a branch: a worktree at a
detached HEAD with no branch of its own. It starts from a commit, a tag, a
branch or a PR's head, a fork's included. Comparing with another commit,
researching older code or reviewing a fork's PR thus never disturbs another
worktree.

It takes an agent of its own as a branch workspace does, through the same
launch, queue, ready, completion and cleanup paths. That agent's opening says
the workspace has no branch, so it cannot push or open a PR from there.

Without an agent, whoever created it runs things in it, such as a CI job, a
reviewer, the parent's agent or the user's shell. Everything else goes through
the workspace paths: the seed clone, executions, leases, scope tokens,
children, holds, checkpoints and removal.

A fork PR's head opens in one, placed as `create --no-branch` places one. Shoal
reuses that workspace for the PR, and moves it to the PR's new head while it
holds nothing of its own.

Its starting commit counts as kept on removal, since a fork's head or a local
commit is on no remote. Commits made on top count as unsaved work. A restore
reopens it without a branch, and branch-only commands refuse it.

### Creation options

`--skip-setup` and `--disposable` are general creation options of `start` and
`create`, not special cases for a runner:

- `--skip-setup` skips what gates readiness, with the removal hooks that tear
  it down.
- `--disposable` makes every removal delete the workspace without inspecting
  it, asking or keeping a checkpoint.

## Copies

### Two flags

A workspace can start from a copy of another's current files, `--copy`, or
from one of its checkpoints, `--checkpoint`. They are two flags because spaces
are also named by number. Neither combines with `--base`, which keeps meaning a
Git ref, and so committed work only.

A copy is an ordinary workspace, on a new branch or at a commit. It has one
editing agent of its own: a fresh one, or with `--fork` one forked from the
source's.

### Built on checkpoints

A copy of current files gets a linked worktree that Git creates without
checking anything out. Shoal gives it the source's index and clones the
source's files, ignored ones included. These replace the seed and
`setup.copy_ignored`. A plain folder copy would share the source's `.git` file
and index.

The copy then takes a checkpoint of itself, which is private at that moment.
The record thus matches the copy's files even when the source's agent kept
editing during the clone.

A copy of a checkpoint writes the checkpoint's files into a new worktree, as a
restore does.

### What a copy was given

Either way, the copy's starting checkpoint keeps every unsaved file. The copy's
record keeps that checkpoint from expiring. Commits up to the copy's HEAD count
as kept, and files that match the checkpoint count as unchanged.

What a copy was given is thus never its own work, and cleanup removes an
untouched copy as it removes an empty workspace. That decides only whether
cleanup may remove the copy. Its branch goes only once a remote or the default
branch has its commits, as any branch does.

A copy takes nothing that belongs to the source's work, as [What a copy
takes](../reference/create.md#what-a-copy-takes) lists.

Agents copy only within their scope. A removed workspace's checkpoint stays the
user's to copy, as reopening the workspace does.

## Refresh

`shoal refresh` brings a workspace up to date with what its record says it was
made from: its upstream or the branch it builds on, a PR's head or a branch's
tip for a workspace at a commit, or a copy's live source.
[Refresh](../reference/branches.md#refresh) says what each moves to.

A refresh moves a branch workspace's branch only by fast-forward. A copy's
branch follows its source's HEAD even after a rebase, since the copy holds no
commit of its own, and a checkpoint keeps its old HEAD first.

A refresh refuses the workspace's own work, naming it and the command that
deals with it. It never rebases, merges or carries the work over: resolving
those is the agent's or the user's work, and a refresh must never lose any.

Ignored files are nobody's work, as cleanup counts them. A refresh of a copy
thus replaces its ignored files with the source's, which are mostly setup
outputs.

A refresh runs only when asked, since a checkout that moves under a working
agent would surprise it. Like a restore, it refuses while another command or
agent runs there.

## Conversion

### Why spaces convert

A space changes kind with `shoal convert`, since only its agent learns what the
work is, and a merged workspace's conversation can still be worth keeping.

A start without a branch thus begins as a chat in its mode, whose agent
converts it once it knows the work. A workspace on a placeholder branch would
keep a meaningless directory name.

### The same space

One transaction changes the kind that the space's `spaces` row records, with
its ID, number and name, and swaps the row of its kind's table. What belongs to
the space by ID thus stays: its links, tree, threads, files, progress and the
places Shoal opened for it. The agent carries on in the place it ran in, where
the user may be watching it.

A chat becomes a workspace through the creation path that every workspace on a
branch takes. The chat's record turns into the workspace's at insertion, and
comes back if the worktree cannot be made. The worktree's directory comes from
the branch.

A workspace becomes a chat through the shared removal path. That path keeps a
checkpoint, the branch and the spaces below, and commits the chat in place of
the workspace's record.

### The agent moves after its turn

A conversion moves its agent only once its turn ends, so the command that asked
for it finishes. The daemon records the pending conversion, which survives a
restart. Its row keeps what queries filter on, the kind of space it makes and
the preparation the agent waits for, in columns, and the rest in a record that
only Rust reads.

One task watches agent states. It stops the agent and starts it again in the
new place from a fork of its session, as `start --fork` and a copy's restore
do. A session resumed in another directory would not find its files there.

### Handing work to another tool

`shoal convert` also hands a space's work to another agent tool, so work goes
on when one tool's usage limit hits. It takes the same pending conversion and
relocation, since the running agent's turn must end first. A stopped agent has
no turn to end.

Context reaches the new tool as copies of the earlier sessions' files in the
space's attached files. Shoal never translates one tool's session format into
another's, which it never reads. The new tool starts a new session, told where
the copies are. That holds for a tool the space used before too, since its old
session would miss what happened since.

The copies outlive the tools' own files and moves between machines. Shoal
copies a session again only once its files changed, so repeated handoffs keep
one copy of each.

A handoff settles every refusal before it stops the leaving agent. It writes
the new tool's agent beside the space's last sessions first. Once the leaving
agent is discarded, only renaming that file into place and recording the
handoff remain, so a failure there leaves the space no agent only when the
file system or the store fails.

A handoff is recorded with the session it left. The new tool's first session
report completes it, since that is when the new session is known. The report
gives the new session's `continued_from` and journals the handoff, so apps show
both transcripts with the tool of each.

Switching tools automatically on a usage limit is a proposal for a later
change.

## The crate's boundary

The `Spaces` service keeps the records of workspaces and chats and the tree
they form. It keeps their marks and progress, worktrees, checkpoints and the
files they send. It runs their lifecycle, removal, restores, conversions, moves
and launches, the cleanup sweep's own steps and the startup audit of what they
own.

The crate's interface reaches the agents, links, repositories, hub, CI jobs,
reviews, leases and demos, and gives the service coordination's service to
call. The service reads and writes what other subsystems record of a space
through the interface's record functions, in its own transactions. It reads
executions through the store's shared readers.

The daemon keeps the cleanup sweep's order across subsystems, the request
dispatch with each request kind's target check, and the connections requests
arrive on. It gives an operation that streams its answer, such as a diff, a
file or a followed session, the writer its frames go to.
