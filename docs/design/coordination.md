# Alerts, threads and changes

This file covers `shoal-coordination`'s alerts, threads, attached files and the
record of what agents change. [Alerts](../reference/alerts.md),
[Threads](../reference/threads.md) and [Attached files](../reference/files.md)
say what each command does; this file gives the reasons.

## Alerts

### One record

Messages to agents and alerts to the user are one record, an alert. An alert is
a one-way notice. It records its typed kind, its sender and its recipient. The
sender is the user, a space or Shoal, and the recipient is the user or a space.
One set of commands thus sends, lists and withdraws alerts in either direction,
and threads build on the same record.

### Routing

One daemon operation decides whom an alert reaches. Without a target, an alert
goes up, to the coordinator or else the user. With one, it goes down, to spaces
the sender's scope reaches. Configurable routes can later replace that one
operation.

Scoped processes read their own space's alerts. They send alerts down only to
the agents of the spaces below their own, which they coordinate.

An alert sent up is one line, since it may reach the user's terminal
notification. An alert sent down may span lines, because instructions to an
agent often need several.

### Shoal's alerts

Shoal sends its own alerts, each naming the space it concerns. It tells the user
what they would otherwise miss, such as a busy resource and who holds it. It
tells an agent what it needs, such as an answer, a decision or a review.
[Shoal's alerts](../reference/alerts.md#shoals-alerts) lists them.

Recording one of Shoal's alerts never fails the operation it describes.

### Alerts stay in the terminal

The CLI shows the user's alerts once, unless asked to keep them unread. It shows
them on request, in a picker, or as a followed stream that raises the terminal's
own notifications (OSC 9). `shoal status` only counts them. Shoal has no desktop
or push delivery of its own.

### Alerts and completion

An alert is separate from completion. Telling the user a PR is ready neither
ends the assignment nor affects cleanup.

What a finished assignment found travels with its completion instead.
`done --summary` records one line, under the rule for an alert sent up, and
ends the completion notice with it. Longer findings are files handed back with
the completion.

### Withdrawal

The sender may withdraw an alert while it is unread. Once it is read, it
cannot: deleting it would not unsay it, and a correction does.

### Collapsing and retention

Identical unread alerts collapse into one until read, repeated polled
conflicts among them.

Each cleanup sweep deletes the user's alerts, read or unread, once they are
older than the retention period. An alert left unread that long no longer
concerns the user, and an unread count that only grows hides the new ones.
[Limits and
retention](../reference/alerts.md#limits-and-retention) says which setting
applies.

Scoped processes read the user's alerts only as their `notifications`
permission allows.

### The agent-exit hook

`hooks.post_agent_exit` shows user integrations a tracked agent's exit, beside
its alert. [The agent-exit hook](../reference/completion.md#the-agent-exit-hook)
says when it runs. A failed hook raises an alert. It changes neither the exit's
result nor completion.

### Alerts on a hub

On machines joined to a [hub](hub.md), the user's alerts are one set. The
machine that records an alert keeps it. That machine stays the alert's
authority, also for collapsing, retention and withdrawal.

Every machine shows every machine's alerts. Each names the machine that keeps
it. Lists order alerts by when they were recorded rather than by number, and a
followed stream keeps its place in each machine's numbers.

Reading or clearing another machine's alert goes to the hub, or waits for the
link while it is down. The hub's copy takes the change at once. The hub passes
it to the alert's machine, while connected or in the answer to its next report.
The copy keeps the change until that machine reports doing the same, so no
report sent meanwhile undoes it.

A clear covers exactly the alerts the clearing machine knew of. Alerts an
offline machine records after it stay.

Withdrawing an alert goes to its machine, found by its number among the copies.
A sender that moved since thus still finds it. While that machine is offline,
the withdrawal fails at once and names it: a withdrawal that waited could remove
an alert after the user read it.

### Alerts to many agents

`send --all` and `--repo` are meant for all work in progress, such as telling
agents the network works again or having them rebase onto a fix. They reach
running agents and those stopped, queued or waiting for their restore, which
read the alert once they start. They skip a workspace without any agent, which
would hold the alert until its removal.

A workspace being removed takes no alert. Removal decides from the alerts the
workspace has and deletes them with it.

An alert to a chat goes the way the user's own messages to it go. Its files stay
among the chat's.

### Files sent with an alert

The daemon copies an alert's files from paths the CLI checked. It copies them
into the repository directory beside its workspaces, for two reasons:

- Agents get that directory as a working directory, so a sandboxed agent reads
  the files there without a prompt.
- A copy in the worktree would be an untracked file that holds up cleanup.

A copy outlives its alert, since the agent's transcript keeps its path. A copy
is named by the alert and the files' contents. An identical alert with the same
files thus still collapses.

## Delivery to agents

### Pushed, not polled

Shoal pushes alerts and item activity to agents, because agents forget or
misuse watches. Built-in agent launches register a hook that delivers both
after each tool call and prompt. That is the only point where a working agent
accepts new context. [Delivery hooks](../reference/alerts.md#delivery-hooks)
says what the hook delivers.

A subagent's tool call delivers nothing, since the agent never sees that
context. A delivery replaces a running watch, so nothing arrives twice.

Agents also read alerts through Shoal commands, and a pending watch ends for a
new alert. An alert to a space waits until its agent reads it, which marks it
read as reading marks the user's.

### Settling

A space's alerts reach its agent once they settle. Otherwise a run of alerts in
quick succession, such as the user deciding several approvals, would cost a
wake or a hook output each. Deliveries hold unread alerts until none has
arrived for a configured window. The window is bounded, so a steady stream
still arrives. Reading alerts with `alert list` does not wait.

### The hook never waits

The delivery hook never waits for cleanup or an item change, which can hold
Shoal's item records for minutes. An agent discards the output of a hook that
outlasts its timeout, so the activity waits for a later delivery instead.

### Waking an idle agent

Claude Code also runs a background hook after a turn ends, normally (`Stop`) or
on an API error (`StopFailure`). It waits for the next update and wakes the
idle agent with it through `asyncRewake`.

The hook tries a failed wait again instead of exiting. A hook that ends leaves
the idle agent with no wait until its next turn. Its waits outlast a daemon
restart, including the time before the next daemon listens and knows the
agent's scope.

The hook exits with Claude Code's process. Claude Code runs hooks outside its
process group, so stopping the agent would otherwise leave the hook behind.

The daemon logs how a wake hook's wait ends without updates. The hook reports
the reasons only it sees, such as its agent's exit or a signal. A missed wake
can thus be traced.

### Wake prompts

Codex has no hook that can start a turn. Its tracked wrapper therefore types a
fixed prompt into the place it runs in, where its host types into its places.
It does so once Codex is idle with updates, at most once per idle stretch. The
prompt's hook then delivers the updates.

Claude Code's wake hook does not run after an interrupted turn. Its wrapper
prompts it the same way while it idles after one. The wrapper also prompts it
once no wait for its updates has run for three minutes. That is longer than the
hook waits between attempts, and an idle agent whose hook ended for any reason
would otherwise wait for the user.

### Idle wakes

A missed wake, a needless question or a restore can still leave an agent idle
with work outstanding. A mode's `idle_wake_minutes` backs the wakes up. An
alert wakes an agent idle that long, and again after each further period
without backoff. The alert reaches the agent through the same delivery as any
update. The daemon tells idleness by the state the agent's hooks report rather
than by separate detection.

An agent that legitimately waits says so with
[`shoal sleep`](agent-lifecycle.md#timers-and-shoal-sleep). A done space is
never woken. Of the built-in modes, only `worker` sets `idle_wake_minutes`,
since the chat modes wait for the user by design.

### What Shoal types into a terminal

Besides the wake prompt, Shoal types only two things into an agent's terminal:
the interrupt key that `shoal stop --interrupt` sends, and the fixed prompt
with which `shoal resume` continues the interrupted turn. The daemon never
types. The prompts carry no forge text, and Shoal appends them to anything
typed but not yet sent.

Shoal does not pre-trust its hooks where the agent requires their review.

## Threads

### Threads and approvals

A thread is how a space or the user asks for something whose answer is text.
An approval's answer is yes or no.

A thread is a conversation between its opener and its holder. The opener is the
user, a space or Shoal. The holder, whoever is asked now, is the user or a
space. Both sides post, and so may any other space that reads the thread. A
question can thus be discussed before it is settled, asked downward by the user
or a parent, and passed on to another agent.

### Kinds and outcomes

A thread has a typed kind, which decides the outcomes it takes:

- A `question` ends `answered`.
- A `request` asks its holder to do something, and ends `done` once its opener
  accepts the work.
- A `review` thread is a review's finding or comment, and takes the outcomes
  [reviews](reviews.md) give it.
- Every kind may end `duplicate` of another thread, `withdrawn`, or `closed`,
  which only Shoal sets.

Shoal refuses unknown kinds and outcomes rather than keeping them as text.

### State in editors

Editors draw a thread with a state that comes from what Shoal records, not
from a field of its own:

- `working` while its holder's agent has switched to it;
- `queued` while its last post from a side is its opener's;
- `waiting` while that post is its holder's;
- else its outcome.

### Opening and posting

A space opens a thread with its coordinator, else the user, or with a space
below it. The user opens one with a space.

An empty thread notifies no one and keeps nothing, so `thread open` only
reserves its number.

A space posts on the side it took part on. It posts on the opener's side as
its opener. It posts on the holder's side as its holder, a space above the
holder or an earlier holder, so a coordinator that passed a question on still
answers it.

Any other space that reads the thread posts as a participant, on neither side.
A participant's post leaves the turn where it was. It offers no surface and
answers none. It links the thread for its space, so that space hears the
replies.

A thread lives on the machine of the space it is numbered in. Its holder may run
on any machine of the hub, which carries the posts between them.

### How a post arrives

Each post reaches the other side's current party as an alert. It also reaches
its own side's party when someone else posted for it, and both sides' for a
participant's post. It goes through the alert delivery that hooks already use,
so it wakes an idle agent. The transaction that records the post queues the
alert, which names the thread as its subject.

`--wait` waits for the next post from another side and prints it, marking its
alert read. It serves agents without Shoal's hooks. A wait that an upgrade
interrupts resumes after the same post.

### Surfaces

A post's text is Markdown. A post may also offer a surface: an interface to
pick or fill in, described in [A2UI](https://a2ui.org). Apps render a surface
natively, and the terminal and agents read it as text. A surface is the only way
a post offers something to pick. `--choice` is shorthand for a standard one.

Shoal pins one A2UI version, 1.0. It vendors the version's schema and records
the version on each surface. Moving to a later version is an ordinary change,
whose store migration rewrites the stored surfaces.

A surface is one `createSurface` message that carries its components and data
model, never a stream of messages. It never changes once posted: an agent shows
new state with a new post.

A surface uses only the components of A2UI's basic catalog, so every app
renders every surface. Its images name files attached to the post. Code and
changes stay on the post as `--code` and `--changes`, which Shoal follows as
code moves.

The daemon checks a surface when its post arrives, and refuses one it cannot
render, naming what is wrong. [Surfaces](../reference/threads.md#surfaces) lists
the checks.

### Answers

An answer is a post that names an action of a surface: the event of one of its
buttons. It gives values for the inputs the action's context binds. Shoal never
reads an answer from text.

Only the other side answers, as for a reply, never a participant. Only the
latest surface a side posted takes actions. An answer to an older one is
refused, naming the current one.

The answer records the action's whole context: the values given, and the data
model's for the rest. It is the event an app's renderer would send.

### Forwarding

The current party of either side forwards its role. The holder forwards its
own; a coordinator that takes over its child's question forwards the opener's.
A role goes down, to a space the forwarder's scope reaches, or up, to its
coordinator or the user. It never goes to the other side's party.

The new party reads what came before and receives the posts from then on.
Earlier parties still post, but no longer forward or resolve.

The forwarder stays linked unless it unlinks, since passing a question on rarely
ends its interest in the answer.

A forward is a post that names the new party, with its note as text. The
thread's history thus shows who took part. A forward does not count as an
answer.

A new opener numbers the thread in its own space, or the user in the holder's,
so the thread's waiting and lifetime follow the opener. The holder and the
linked spaces hear the new number. A thread does not move between machines, so
an opener's role goes only to a space of the thread's machine.

### Resolving

The opener resolves its thread. The holder resolves a thread Shoal opened. The
user may resolve any thread. `--undo` reopens a resolved thread.

Withdrawing takes back the holder's unread alerts of the thread, so nobody is
asked to decide what no longer matters. The other side hears of any other
resolution once it has heard of the thread.

### Reading and linking

A space reads a thread it took part in: as the space the thread is numbered
in, or as its opener or holder, now or earlier. It also reads one a space its
scope reaches took part in. A space that reads a thread may link it to receive
its new posts. Reading delivers nothing.

### Waiting threads

A thread waits while it is open and its last post from a side came from the
opener's. A waiting thread keeps the space that opened it from unattended
cleanup, since an agent waits on it. It also ranks that space's queued work
after the holder's. The inbox lists the threads whose turn is the user's.

### Completion and removal

`done` closes every open thread the space is numbered in, opened or holds, on
any machine, since finished work waits on no answer. It tells an opener other
than the space that the holder finished. Removing a space does the same.

`done --undo` and a restore reopen the threads closed for that cause, without
Shoal's closing posts. A thread something resolved since stays resolved.

## Code references

### References belong to posts

A post may point at code: a file or its lines in the worktree of a space the
poster's scope reaches, or of the workspace its chat runs in. A reference only
reads the worktree.

References belong to posts rather than threads. A request about several places,
a reply that points elsewhere and an answer that says where something should go
all attach code mid-conversation. The opening post's references are the
thread's place in code.

A reference points at the worktree's file and does not copy it. It keeps the
file's version as a Git blob, HEAD at the time and a snippet of its lines. The
snippet shows once the code is gone.

### Following code

Shoal follows a reference as its code changes. When a read finds that the file
differs from the version the reference last reached, Shoal diffs that version
against the file now and records where the lines went. It never maps from the
original: many small diffs keep a range exact, where one diff after heavy
editing can misalign it. There is no background pass.

The mapping is a pure function over two texts in `shoal-protocol`. An editor's
language server thus maps a range onto an unsaved buffer as the daemon does
once the buffer is saved.

Git's rename detection finds a file renamed whole. Shoal does not search for
code moved into another file.

### Where versions are kept

The repository keeps the versions under one ref per space. The ref's commit
holds in its tree each blob the space's references keep. A ref per blob would
slow every Git command that lists refs.

As references go with their threads, the expiry pass rewrites the ref. It
prunes the objects as checkpoint expiry does.

## Agents' changes

### Why Shoal records changes

Shoal records what the agents it runs change. A thread thus shows the code its
work changed, a review reads per request, and editors read one record, also
after they restart.

Shoal attributes changes itself, and the agent corrects what it cannot tell.
Detecting the writing process instead needs root, or an Apple entitlement on
macOS. Library injection misses the system binaries that shell commands use.

Agents without Shoal's hooks report no tool calls, so they get no record. A
chat changes no files of its own.

### Edit tools

An edit tool's hook passes the edit. Claude Code's Edit, Write and MultiEdit
pass their structured patch, whose hunks are the agent's. The user editing the
same file meanwhile thus does not confuse the record. A hunk the file no longer
holds where the tool put it is mixed.

### Snapshots

A tool whose payload carries no lines is snapshotted before and after it runs.
Shell commands, Claude Code's NotebookEdit and Codex's `apply_patch` are such
tools. A snapshot is the tree of the worktree's files that Git does not ignore.
Shoal writes it through a temporary index, as it writes a checkpoint.

What changed between the two snapshots is the agent's. It is mixed in a file an
editor reported the user edited meanwhile.

Each space keeps one snapshot, which its next change is diffed against. An edit
tool's version updates it. What changed between the agent's tool calls, such as
the user's edits or a formatter's, is thus someone else's. While one command
runs, every change is that command's.

### A change

A change is one hunk. Changes are numbered in a sequence of their space's own,
apart from `S.N`, so an agent can correct one after its turn.

A generated, binary or large file is one change without hunks. It counts in no
size and is never asked about.

A change's lines follow its code through the same mapping references take. They
stay valid once the agent commits the change.

### Authorship

Most changes belong to no thread, so authorship is its own group of commands:
`shoal changes` lists, claims and unclaims changes. After a command, the hook
lists the mixed changes it left and asks once. Nothing waits on the answer.

### Changes for a thread

The agent says which thread its work is for with `thread switch`. A request can
take several turns, and Shoal cannot infer the thread from the posts it
delivered. A switch lasts until the next one or until the thread is resolved.

A change belongs to at most one thread: an open one its space takes part in, of
any kind.

A thread's history shows an entry per turn and agent between its posts. A
post's alert sums up the changes since the sender's last post. A post may point
at changes as it points at code, which moves nothing between threads.

### Expiry

Changes in a thread go with the thread. The others expire as checkpoints do.

## Posting from the chat

### In-chat posts

An agent posts in its chat with `send <thread> --in-chat`. The turn's final
message, which the `Stop` hook passes, becomes the post. A user talking with
the agent reads the post as the agent wrote it, where a long command line in
the chat is hard to read. A user who is away gets the post where they look.

A prompt the user submits in the chat becomes the user's reply to the post,
carrying its text, since the user answered there. A prompt Shoal submits does
not.

### Attachment does not tell presence

Shoal cannot tell from a host whether the user is there. zmx counts its own
short-lived probes as clients, and an attach from a connection left open counts
while the user is away. Posts routed by attachment would thus go unanswered.
Agents are therefore not told about attachment, which only orders overload
stops.

### In-chat alerts

`send --in-chat` without a thread makes the turn's final message an alert, for
the same reason. It takes the path every alert takes.

The command resolves and checks the alert's recipients and files when it runs,
so a mistake fails at once. It copies the files when the turn ends, as the
agent left them.

A turn has one mark, a post's or an alert's. A second mark replaces the first
and says so, rather than sending both.

A prompt the user submits during the turn drops an alert mark, since the final
message then answers the user. Shoal tells the agent, which may send it again.
A prompt in the chat marks the chat's in-chat alerts to the user read. It
records no reply, since alerts are one-way.

## Numbers

Threads and approval requests are numbered `S.N`: the number of the space they
belong to, then the smallest number no thread or request of that space holds.
A thread belongs to its opener's space, or to the space the user or Shoal
opened it with.

Both kinds share the space's sequence, so a number names one item and says
which space it belongs to. Counting per space keeps the numbers short, because
the user types them.

Inside a space, a bare thread number names one of that space, the common case
for an agent. Elsewhere, and for approval requests, Shoal refuses a bare number
or one of the other kind rather than guessing. It names the form or command to
use. The records keep a separate internal key that is never shown.

A thread keeps its number until its space goes, resolved or not. A late post is
thus refused instead of reaching a later thread. A wait follows its thread's
space by ID, which a renumbering leaves alone.

A request that binds a commit and no longer applies stays as outdated until its
workspace goes. Its number is thus never reused, and a decision meant for it
cannot approve a newer commit.

A proposed issue outlives its space. The space's number thus stays reserved
until the last such proposal is decided, keeping the number the user was told.

An agent may withdraw a pending request it no longer needs, as it may a thread.
The request stays as withdrawn on the same terms. A proposed issue is the
exception: it would outlive the workspace, so it goes as a decided proposal
does.

## Attached files

### Chats own files

A chat owns attached files as a workspace does, since its agent hands the user
reports and pages too.

### Retention

A removed space keeps every attached file by one rule, whatever its kind. The
files stay for the same retention as its checkpoints. An agent's restored
session may refer to any of them. A chat has no worktree to clean up, so its
files are what remain of it.

### Files sent with an alert or a post

Files an agent sends the user with an alert or a post join the space's
attached files, since the user may read the alert after the space is gone. They
outlive the alert or post, as a message's files outlive the message. Clearing or
withdrawing the alert, or resolving the thread, leaves them among the space's
files until `shoal files rm` or the retention removes them.

The alert or post names its files by their records rather than by path. A file
`shoal files rm` deleted thus drops out, and a moved repository directory moves
the files. A coordinating agent that receives the alert or post receives their
paths instead, as the message's attached files.

### Fetched copies

A copy fetched here of another machine's file stays for the same retention
after its fetch or last change. Its age alone decides. This machine cannot
always learn when the source's space goes, and a fetch happens only while that
space exists. An unchanged copy thus never outlives the source's retention,
while a later fetch of a living space's file makes it again. An edit keeps the
copy that long again, since its changes exist nowhere else.

Disk space cleanup deletes fetched copies first, since their source machine
keeps them. It spares a copy the user changed since its fetch and one a fetch
is still writing.

### Owners

Files are keyed by their owner's ID, which a workspace and a chat never share.
No kind tells owners apart: an owner is gone when neither kind has the ID. A
record of its own says how long its files stay. It is apart from a workspace's
removal record, which a disposable workspace lacks.

### Who attached a file

Each attached file records who attached it: the user, or the agent whose
process tree sent the request. It names the agent by two things:

- its execution, which pins the run;
- the session it reported, which outlives a restart or resume and names the
  conversation a client shows the file in.

A chat's agent is recorded as a workspace's is, since it runs as an execution
of its chat. Like scope, this is cooperative.

### The journal

The journal records each file's record as it is added and deleted. Store
triggers write it, so no route of attaching or deleting is missed. The journal
carries the owner's ID from the record, because a removed space has no row
left to supply it. Clients showing files follow the journal without polling.

### Titles

A file's optional title rides on its argument as `PATH#TITLE`, the form forge
attachments use for alt text. Attach flags repeat, and a separate title flag
could not say which file it names.
