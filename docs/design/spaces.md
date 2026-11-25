# Spaces

This file covers `shoal-spaces`' workspaces and the tree of spaces: creation and
launches, stacking, adoption, commits, copies, refresh, conversion and numbers.

`shoal start`, `create` and apps open a workspace and start its preparation
through one request, `StartWorkspace`, `StartCreation` or `StartChild`, which
take the steps every new space's start takes, whatever its kind: it refuses an
agent that cannot run, an issue that is not open or a branch that is not a
literal Git branch name before creating anything, records a new workspace,
`preparing` without its worktree, or creates a child's with it, or reopens the
one on the branch, PR head or record it names, assigns the issue, removing a new
space it cannot assign, queues the detached agent the request asks for, with its
mode, model, arguments, fork, issue and caller's place, as a scoped `start
--agent` starts its agent, which waits in the queue for the preparation, and
starts the operation, which creates the worktree, sets it up, links its PR and
runs a new workspace's post-setup hook. A preparation that creates a new
worktree waits first, in a record beside the workspace, for the blockers of the
agent's launch, then creates it on the branch of the done blocker that holds the
others' work, or from the default branch when none leaves one, so it never
starts behind; a start nothing follows also waits while disk protection holds
back new starts. The queue's dispatcher starts it once that allows, in the scope
of the space that started it, and a stopped agent leaves it waiting for an
explicit preparation. Before any process runs an admitted launch's agent, the
daemon opens its workspace for it: the workspace must still be its ready,
verified worktree, a branch that holds nothing of its own fast-forwards to what
it builds on, as `shoal refresh` does, so its agent starts on a current branch,
and the PR the start names is linked, a refusal warning the starter; a copy is
made when its preparation runs, which waited for what it waits for. A command,
merge or unmerge whose start falls in such an update, or in any lifecycle
operation that returns the workspace to ready or failed on its own, waits for it
to end and is prepared again for the workspace it leaves: the window is Shoal's
own, not a reason the command cannot run. The store counts each workspace's
lifecycle changes, so a start whose registration fails after such an operation
came and went since its first look, which can leave the worktree unverifiable
meanwhile, is prepared again too. Removal, a failure and setup, which may wait
for other work first, still refuse it, as such an operation refuses an agent's
registration, which holds the queue's admission that a stop of its workspace
needs. The request answers once the workspace is recorded, so a caller gets its
number and its agent's entry without waiting for Git or setup; until its
worktree exists, what reads it refuses the workspace, and removal takes only its
record and leases. The way the start's caller hears other news it does not wait
for, a failed preparation reaches the agent of the nearest space above the
workspace with one, or else the user, naming the restore that retries it and the
removal. Every other creation, a split, a conversion, a restore, a copy, a
commit or CI job's workspace and `adopt --copy`, also records the plan of its
worktree with the workspace and creates it through the same path, only at once,
so a failure at any step leaves a workspace the restore retries. A workspace's
record says whether Git created its worktree: a pending one keeps its plan and
what an earlier try recorded, and the store refuses a created one without its
base and admin directory. Git creates a worktree before Shoal records its admin
directory, so a retry or removal of a pending workspace without one takes the
admin directory whose `gitdir` names its path, unless another workspace marked
it; one without has no worktree. An add Git still marks as under way was cut
short before its checkout finished, so a retry deletes it and adds the worktree
again while its directory holds only the `.git` link the add wrote, and refuses
while it holds files, which may be someone's work; a retry that writes files
into a worktree an earlier try left keeps what it holds in a checkpoint first. A
restore whose worktree fails still opens the space with its links and agents,
the workspace failed, so the retry needs nothing of the removal record. Only a
created worktree is checked for a move when it goes missing, so after a `git
worktree move` during a cut-short creation `shoal rm` forgets the record,
keeping the branch and the moved files. The request names an agent, mode or
model only where the caller chose one; the daemon resolves the rest under the
workspace's settings, `agent.default` for the agent, and the agent works on the
start's issue unless the request gives it an issue or a task, so an app needs no
configuration of its own to start one as the CLI does. `QueueAgent` queues the
worker of an existing workspace the same way, on the workspace's issue. Every
start of a new workspace or of an existing workspace's agent, whoever makes it,
goes through one request, `Launch`, which carries what `shoal start` takes, as
its command line gave it, and what another command's start adds, such as the
subject a repository's setup gives its agent and the setup mode, which no other
launch starts in; the daemon records a client's launch as `manual`, and names
the source of a launch it makes itself. The daemon alone decides what the launch
leaves out, with the steps in `shoal-core`: the mode, the kind of space and the
repository, which the launch names, its issue or link belongs to, or the
checkout or worktree the start was made in has; and the branch a PR, a branch
link, the typed name or earlier work on its issue settles. It then queues the
worker of the workspace `--in` names, or of the one that has the branch, as
`QueueAgent` does; or starts a new space, a chat for a launch without a branch
or issue, through the steps `StartWorkspace` takes: the kind decides only where
the space goes, a chat's checkout, workspace worktree or `root_dir` or a
workspace's new worktree, and the daemon reads the issue the same way for both,
resolves the agent and mode under the settings there, names the space as the
launch names it, else after its issue's or PR's title or where a chat runs,
records it, links the issue, queues its agent through the queue's one request,
waiting for what blocks it, and prepares a workspace, the only step that depends
on the kind. Every launch answers with the space it started in, of either kind,
as it created or found it, and how it started the agent there, so a caller reads
the space without asking its kind; a background place's worker reports only
where the place runs. The queue entry of the launch's agent carries its source
to the agent's start, which the journal records with it. The CLI parses the
start, picks on a terminal what a picker picks there, adds what only its process
knows, such as its directory, its environment and the session `--fork` forks,
and what only its terminal can check, such as a host to place the agent in; it
keeps what attaches a terminal: a start here, a focused start that opens its
place beside the terminal and shows the preparation there, and an agent that
runs its own way, such as Happy's, which starts in the caller's process for a
chat as for a workspace. Such a launch says so with `here`: the daemon queues
the agent as for any start and keeps its entry for the caller, which holds it
while it follows the preparation and then claims it; the claim starts the agent
in the caller at once where the start asked for that, else when the limits admit
it, and otherwise leaves it to the queue's background start. Every queue entry
for a new workspace belongs to its recorded space, and the queue's start runs
the agent in it. Any other start reports the recorded workspace and its agent's
entry without following the preparation, unless `--wait` asks it to. One
`shoal_client` call sends the request and follows the preparation, returning the
prepared workspace or the typed failure with the workspace as it left it, ready
when only what follows setup failed. The CLI makes that call and keeps its
prompt for a failed setup and an agent it launches in the terminal. An interrupt
or termination of the CLI cancels the operation, which interrupts the worker as
it interrupted the CLI; a CLI that ends otherwise leaves it running. The daemon
keeps an operation in memory until ten minutes after it ends, so a client
follows it again by its ID from any event; a restart forgets it, and setup and
merges fail with their daemon as before.

A stacked workspace records the workspace whose branch it builds on, inferred
when the creation base names another workspace's branch locally or on a remote,
so stacks span workspaces on any forge. The base is local metadata, not a link:
removing it moves its stacked workspaces down to its own base and journals the
change. When every watched PR of the base merges and covers its HEAD, the PR
sweep does the same before cleanup can remove it, retargets stacked PRs still
targeting the head branch of one of its merged PRs, and queues a watch update;
agents rebase their own branches, since Shoal moves a workspace branch only by
the fast-forward `shoal refresh` makes when asked, or the start of an agent the
queue admits there makes. Retargeting goes through the same REST transport as
issue and PR actions. Adoption takes a path; the CLI infers its repository from
the owning checkout, then a unique `origin` remote match, then a picker, with
`--repo` as the override. Explicit adoption accepts an unlocked linked worktree
root on a local branch of the registered repository with no ownership conflict,
preserving dirty files and Git settings and recording it ready without setup or
hooks. It takes normal cleanup ownership. Reopening verifies its record;
adoption cannot repair a moved managed worktree, and repairs one whose metadata
was replaced only by reclaiming it at its recorded path. Never adopt a main
checkout. `adopt --copy` records a preparing workspace before creating a fresh
worktree under the repository's Shoal directory, copies the source worktree's
files while leaving the source in place, removes checkout files the source
deleted at any depth, and adopts the copy on an available branch derived from
the source branch; a failure keeps its plan, whose retry copies the source again
and fails naming it once it is gone.

A workspace may sit at a commit instead of on a branch: a worktree at a detached
HEAD with no branch of its own, created from a commit, tag, branch or PR head, a
fork's included, so comparing with another commit, researching older code or
reviewing a fork's PR never disturbs another worktree. It takes an agent of its
own as a branch workspace does, through the same launch, queue, ready, completion
and cleanup paths; that agent's opening says the workspace has no branch, so it
cannot push or open a PR from there. Without an agent, whoever created it runs
things in it: a CI job, a reviewer, the parent's agent or the user's shell, with
`shoal run` and `shoal demo`. Everything else goes through the workspace paths: the seed clone, executions, leases, scope tokens, children, holds,
checkpoints and removal. `create --no-branch` creates one, below the caller's
space or as the user's top-level one, and a fork PR's head opens in one, placed
the same way, that is reused per PR and moved to its new head while it has nothing of its own. Its
starting commit counts as kept on removal, since a fork's head or a local commit is
on no remote, while commits made on top count as unsaved work; restore reopens it
without a branch, and branch-only commands refuse it. `--skip-setup` and
`--disposable` are general creation options of `start` and `create`, not special cases
for a runner: one skips what gates readiness, with the removal hooks that tear it
down, and the other makes every removal delete the workspace without inspecting it,
asking or keeping a checkpoint.

A workspace can start from a copy of another's current files or from one of its
checkpoints, with `--copy` and `--checkpoint` on `start` and `create`: two flags,
since spaces are also named by number, and neither combines with `--base`, which
keeps meaning a Git ref and so committed work only. A copy is an ordinary
workspace on a new branch, or at a commit with `create --no-branch`, with one
editing agent of its own, a fresh one or with `--fork` one forked from the
source's. Copies are built on checkpoints. A copy of current files gets a linked
worktree created without checkout, the source's index and cloned files, ignored
ones included, which replace the seed and `setup.copy_ignored`; a plain folder
copy would share the source's `.git` file and index. It then takes a checkpoint
of itself, private at that moment, so the record matches its files even when the
source's agent kept editing during the clone. A copy of a checkpoint writes its
files into a new worktree as a restore does. Either way the copy's starting
checkpoint keeps every unsaved file, and the copy's
record keeps it from expiring, so commits up to its HEAD count as kept and files
that match it count as unchanged: what a copy was given is never its own work,
and an untouched copy is cleaned up like an empty workspace. That decides only
whether cleanup may remove it; its branch goes only once a remote or the default
branch has its commits, as any branch does. It takes its
source's diff base and worktree Git config, and nothing that belongs to the
source's work: links, leases, holds, threads, ready marks, checkpoints or the
session. Agents copy only within their scope, and a removed workspace's
checkpoint, like reopening it, stays the user's.

`shoal refresh` brings a workspace up to date with what its record says it was
made from: its upstream or the branch it builds on, a PR's head or a branch's
tip for a workspace at a commit, or a copy's live source, whose files it copies
in again in place after a checkpoint keeps the copy's. It moves a branch
workspace's branch only by fast-forward, while a copy's branch follows its
source's HEAD even after a rebase, since the copy holds no commit of its own
and the checkpoint keeps its old HEAD. It refuses work of the workspace's own, naming it and the command
that deals with it, rather than rebasing, merging or carrying it over: resolving
those is the agent's or the user's work, and a refresh must never lose any.
Ignored files are nobody's work, as cleanup counts them, so a copy's are
replaced by its source's, whose setup outputs they mostly are. It
runs only when asked, since a checkout moving under a working agent would
surprise it, and like restore it refuses while another command or agent runs
there.

A space changes kind with `shoal convert`, since only its agent learns what the
work is, and a merged workspace's conversation can still be worth keeping. The
space stays the same: one transaction changes the kind its `spaces` row
records, with its ID, number and name, and swaps the row of its kind's table,
so what belongs to the space by ID, its links, tree, threads, files,
progress and the places Shoal opened for it, stays: the agent carries on in
the place it ran in, where the user may be watching it. A chat
becomes a workspace through the creation path every workspace on a branch
takes, with the chat's record turned into the workspace's at insertion and
given back if the worktree cannot be made; the worktree directory comes from
the branch. A workspace becomes a chat through the shared removal path, which
keeps a checkpoint and the branch and the spaces below, and commits the chat
in place of the workspace's record. A conversion moves its agent only once its
turn ends, so the command that asked for it finishes: the daemon records the
pending conversion, which survives a restart, and one task watching agent
states stops the agent and starts it again in the new place from a fork of
its session, as `start --fork` and a copy's restore do, since a session
resumed in another directory would not find its files there. A start without a
branch begins as a chat in its mode, whose agent converts it once it knows the
work, instead of a workspace on a placeholder branch whose directory would keep
a meaningless name.

`shoal convert` also hands a space's work to another agent tool, so work goes
on when one tool's usage limit hits, through the same pending conversion and
relocation, since the running agent's turn must end first and a stopped one
has none to end. Context reaches the new tool as copies of the earlier
sessions' files in the space's attached files, never by translating one
tool's session format into another's, which Shoal never reads; the new tool
starts a new session told where they are, also a tool the space used before,
whose old session would miss what happened since. The copies outlive the
tools' own files and moves between machines, and a session is copied again
only once its files changed, so repeated handoffs keep one copy of each. A
handoff is recorded with the session it left, and completed by the new tool's
first session report, which is when the session it started is known: that
gives the new session's `continued_from` and journals the handoff, so apps
show both transcripts with the tool of each. Switching tools automatically on
a usage limit is a proposal for a later change.

`shoal create` makes a workspace as `shoal start` makes one, from the same
arguments that choose what it starts from and where it goes, and starts no
agent there: `create` is the way to a workspace without one, at the top from
the user's terminal or as a child
inside a space, so an agent whose work needs another PR gets another branch
and worktree instead of switching branches or creating worktrees Shoal does
not own. The daemon places it as a launch's new workspace and records it
through the steps every new space's start takes, with nothing to queue. The
parent is a workspace or a chat of a repository, as for any child. A child goes
in its parent's repository on a new branch from the default branch or a `--base` that
stacks it as usual, records the parent, and links an `--issue`. Any space opens
a child of either kind the same way: `shoal start` run inside a space puts the
new workspace or chat below it, or below a space its scope reaches that
`--parent` names, in its repository, and a scoped caller creates nothing else,
while the user's terminal creates top-level spaces.
The daemon records every new chat, below where it goes and linked to what it
starts on, before its agent runs anywhere, in one operation whatever runs the
agent: the caller's terminal, a background place, the queue or an app only
runs the recorded chat's agent, so a start that names nothing to link sends no
link request. The queue starts every admitted agent the same way, opening a
place for any recorded space, and every agent start has an entry, admitted at
once where no limit applies. Every start sends one launch, a chat's that
runs here or beside its terminal included: the daemon records the space with
its agent's entry, which a caller that runs the agent holds, and the CLI
runs the recorded space's agent through one placement step for both kinds,
here, beside, in a background place or detached, a workspace's once its
preparation ends. One place worker runs any recorded space's agent in a
place, in the space the daemon opened for it, so no other process reopens a
workspace or links what its start names, and one builder makes its command from the space's records, so only
what the record holds differs by kind, such as a chat's home and the
instructions its agent receives first. A start that fails fails its queue
entry. What linking a new space, of
either kind, warns of goes to the agent of the nearest space above it with
one, else to the user's alerts, never only to a terminal the agent then takes over. A space
whose place never starts its agent is kept, whatever its kind, with its
launch as a failed queue entry, which nothing admits and which keeps no
space from cleanup; its failure goes to the starter the same way, naming
`shoal resume <space>`, which queues the launch again as it was, and
`shoal rm <space>`. A new launch in the space replaces it, and the space's
removal drops it. A start the user makes by hand from their own terminal, of
a new space or an existing workspace's agent, and a restore they run in their
terminal record their interaction with the space's agent; a trigger's or a
scoped caller's do not.
A chat an agent starts opens in a background place, which the agent queue
opens once it admits the chat's agent, since the agent cannot hand over its
terminal, unless `start --here` runs it in the caller's own; the place runs
the chat's agent in the chat's own scope, and the start returns once the
daemon has recorded the chat and queued its agent. A chat run in the caller's terminal is the
chat's own execution, as a workspace's agent is, which the caller's stops
leave running; places drop the marks and scope token of the space
that opened them, so what runs there is the new chat's. Spaces form one tree through one parent link,
`space_parents`, which records a workspace created below another and one a chat
opened alike: every space has at most one parent, a ready workspace or a chat
of its own repository, or a chat without a repository, never one below it, and
any other child in another repository is refused: spaces across repositories
share a parent only in a chat without one. Every space has one row in
`spaces`, with its kind and what every space has, which its workspace or chat
row extends with what only its kind has; a conversion keeps it, and deleting it
removes the space with what each table of spaces holds for it. A removal reserves the space first, in memory whatever its kind, and a
workspace's worktree also through its `removing` state, which startup
recovery reads. A hold keeps a space of either kind and stays through its
conversion; a reserved space takes no new hold, so a removal that leaves the
user's holds in place checks them once, when it reserves the space. Every
reader takes one walk up or one walk down that link, with no branch on kind,
and asks each space one question, whether it has an agent: a workspace's
agent, a chat's running agent, or a queued restore of either. A scope reaches
its space and every space below it, and a chat without a repository's every
space, which one operation names for every check, the CLI resolves a scoped
command, a workspace's or a chat's, to the workspace below whose directory it
runs in, else to its own space, and agent hooks always report for the agent's
own space. So a chat's agent that changes into a workspace it opened acts on
that workspace, as a workspace's agent acts on its child. The parent's agent, a workspace's or a chat's, receives the
issue and PR activity of the spaces below it without an agent of their own,
and theirs in turn, each update naming its space, through the same cursors,
deliveries and acknowledgements as its own; a child's own agent, which reports its turn state or which Shoal tracks
there, as it tracks custom and Happy agents that report none, takes over its
subtree, and so does one whose restore is queued, since it receives what
arrived once it starts. A child keeps its parent only from merged cleanup,
which would otherwise take a child's open work because the parent's own
branch merged, and never outlives it:
the parent is cleaned up by its own rules, and every removal of it, manual or
unattended, first removes the spaces below it, deepest first, through the
shared removal path, with a checkpoint and kept branch for each workspace but a
disposable one, whatever their work, holds or threads; one that cannot go
retains the parent, and the journal and
the removal's output name those taken. While a parent's completion with cleanup
stands at its HEAD, its children of either kind go too after the grace period,
whatever their own work, unless one of their own keep-reasons, or a space still
below them, keeps them; a child's own `done` follows the usual rules. The parent owns its
children, so its processes may remove and hold them, except that the user's
holds anywhere below refuse the removal. `list` shows children below their
parent, and `status` names the parent and children of either kind. `shoal start`
is the only command that starts an agent, so `create` creates worktrees alone: a
`start` run in a space, a workspace or a chat, gives a child an agent of its own
through the agent queue, and `start --fork` gives it the parent's agent
from a fork of its session, the one the caller's agent or else the parent's only
agent reported last, so the parent need not restate what it learned. A fork
of the same model reads the parent's prompt cache, but re-reads its whole
context on each of the child's requests and fills its context window sooner,
so `start` keeps a fresh agent as its default, and the shipped `work` rule
tells agents to fork for a short task that needs their context and to start
fresh for long work or a task most of it does not concern; the daemon starts every launch in an existing
workspace itself, detached, unless `start --here` has the caller start it, and the agent
holds its own space's scope, never the caller's, and
scoped callers may queue only for the workspaces their scope reaches but their
own, and cancel and prioritize the entries their scope reaches: those of such
spaces, a CI job's through the spaces its run is for, and a new space's launch
through its parent. A scoped caller's
start that a blocker holds records its workspace below the caller at once, as
any start does, and its waiting preparation records the caller's space, in
whose scope it runs once the work is done. A space's
coordinator is the agent of the nearest space above it that has one, whatever
its kind, which one operation queues a message for: the events about
it that notify the user, the child agent's messages, threads and completion
and its agent's exits and stops, plus its ready marks, become that agent's
messages instead, falling back to the user without a coordinator or when its
messages are full. Events a process causes in a child without an agent of its
own are the coordinator's own work and reach the user. The coordinator answers
the threads opened below it or forwards them up, and may
stop, restart, resume and convert the spaces below it and forget their stopped
work, since only it knows whether a child agent still has work. It resumes a
stopped agent whoever stopped it, the user, Shoal or a crash: agents through
the queue, so the daemon starts them detached, and chats in a background
place, as it starts them. It confirms that an execution Shoal cannot prove
ended did as the user does, while the check of a survivor's birth identity
still keeps unknown ownership from authorizing a kill. Every such check asks
whether the caller's scope reaches the target, and a refusal names the space
outside it.
Stopping a space stops the spaces below it, workspaces and chats, unless
`--skip-children` says otherwise, since their agents work for it. Approval
requests go up the same tree: to the nearest space above with an agent, of
either kind, since each knows best what its children need. A parent space,
workspace or chat, approves what a permission governs where its own
permission for it is `direct`, anything else only where it holds an approved
grant for it, and a hold on its child, and otherwise denies or passes the
request up, unless its `decide` permission lets it decide any request below
as the user would, a request a permission governs still only where it holds
that permission at `direct`; the user
decides at the top, and a chat whose agent stops or a
deciding space that is removed hands the user what it had not decided, since
neither can decide any more. A space lists the requests it decides beside its
own, whatever its kind, since it must find them to decide them.
When a subagent works in the child's directory instead of an `--agent`, the
child has no agent of its own, so its activity reaches the parent, and the
subagent reports to the parent directly, supervision Shoal need not relay. Shoal does not track those subagents: their hooks report under
the coordinator's scope from its directory, so they cannot tell which child a
subagent works in. A child in which no agent of its own has ever started,
which the workspace's recorded mode tells, is instead its coordinator's work:
it lists as `parent working` while that agent works, and idle cleanup keeps it
while that agent runs, in any turn state, since subagents may outlast a turn
and a coordinator may wait for the user mid-delegation. A chat below it whose
agent stopped is its work the same way, so idle cleanup keeps that chat too.
Claude Code's worktree
hooks make the worktree it creates for a subagent such a child, so its own
isolation needs no worktree Shoal does not own.
A subagent inherits its agent's scope, and under Codex its instructions and
directory, so a `SubagentStart` hook names the workspace it works in; without
it, a Codex subagent finishes its agent's assignment as the instructions say.

Issue-based workspace opening requires an open issue and persists its canonical
URL before tracked setup or agent launch. Reopening first follows the issue
association, then an existing local branch with the derived branch name, rather than
suffixing a new branch; conflicts fail before the agent picker. Associations are idempotent and cannot
be replaced. Status and inspection expose them.
The derived branch is the issue number and at most three title words, short
enough to read in full in branch lists, while the workspace takes the issue's
title as its name, which says what the work is.

`start <link>` recognizes issue, PR, CI run and branch URLs and selects their
registered repository by origin identity, offering interactive registration when
missing. Issue links and numbers open the issue's workspace; numbers use
`--repo`, the current checkout/workspace, then a picker. PR links open their
head against the refreshed remote base and link the PR once the workspace is
ready, as issue links associate their issue; branch links open an origin branch.
Both use existing-branch ownership checks and retain an owned workspace's diff
base. A fork PR has no origin branch, so its head opens in the PR's workspace at
a commit instead, with the PR's base as its diff base, where `--agent` starts
its agent, and `--path`, `--git-profile` and `--base` are refused. A CI run
link, or `--ci`, starts where the run ran, read as the caller may link it: its
PR's or branch's link, or for a run on the default branch, a tag or no branch,
which names no work, a start without a branch, which begins as a chat named
after the run; a mode that starts a chat starts one on any run. The run says
nothing of the mode, so a PR's labels choose none. The space links the run as
related and its agent starts on a summary of the run, with its failed jobs but
no log tails, which `shoal view ci` shows. `--ci` without a run offers the
newest run of each workflow and ref that failed, so a failure a later run fixed
is not offered. Interactive `start` without a branch, base or issue also offers
the repository's open issues and PRs, which open as their links do. Every
`start` starts an agent, one rule for a new workspace, a new chat, `--in` and
`contribute`, whatever else it names: `--agent`, else `agent.default`, else a
picker on a terminal, which shows only where no default is set, and otherwise a
refusal before anything is created that names `shoal create`, which makes a
workspace without one. The daemon applies the default and the refusal, so a
start from any client follows the rule. `start --agent` launches only after
creation, setup, and the post-setup hook succeed, or after an explicitly ignored
setup failure, and retains the workspace whatever the agent does.

A workspace or a chat is a space: the user reads both by their names and selects
both by number or ID, so what applies to both is named for the space, and the
flag that selects one is `--space`. Each space also has a number, the smallest
no other space holds when it is recorded, from one sequence across repositories
and a hub's machines, so it stays short to type and read; it lasts as long as
the space, survives renames and stops, and is freed when the space goes. Every
argument that takes a space selects it by number or ID, through one resolver in
the daemon and one in the client; a name is a label people read and never
selects one, so names may repeat. The client refuses text that is neither a
number nor an ID before it asks the daemon. A number selects only a space that
exists, never a removed workspace or a queued launch, which are selected by ID.
`--add-blocker` is the exception: it names blockers, where a bare number stays
an issue, as `shoal start` takes one.
