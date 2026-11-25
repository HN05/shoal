# Alerts, threads and changes

This file covers `shoal-coordination`'s alerts, threads, attached files and the
record of what agents change.

Messages to agents and alerts to the user are one record, an alert: a
one-way notice that records its typed kind, its sender (the user, a space or
Shoal) and its recipient (the user or a space), so one set of commands sends,
lists and withdraws them in either direction, and later threads build on it.
Shoal sends its own, naming the space they concern: what a user would
otherwise miss (busy resources and who holds them, port conflicts, exits of
agents launched in place, workspaces it removed or retained on its own) and
what an agent needs (answers, decisions, reviews, wakes, machine load). One
daemon operation decides whom an alert reaches: without a target it goes up,
to the coordinator or else the user, and with one it goes down to spaces the
sender's scope reaches, so configurable routes can replace that operation
later. Alerts stay in the terminal: the CLI shows the user's once, unless
asked to keep them unread, on request, in a picker or as a followed stream
that also raises the terminal's own notifications (OSC 9); `status`, in a
space and outside one, only counts them. Recording Shoal's own never fails
the operation it describes, repeated polled conflicts and identical alerts
collapse until read, and scoped processes read the user's only as their
`notifications` permission allows. On machines joined to a hub, the user's
alerts are one set: the machine that records an alert keeps it and stays its
authority, collapsing, retention and withdrawal included, while every
machine shows every machine's, each another machine's naming that machine,
in the order they were recorded rather than by number, and a followed stream
keeps its place in each machine's numbers. Reading or clearing another
machine's alert goes to the hub, or waits for the link while it is down,
whose copy takes it at once and which passes it to the alert's machine,
while connected or in the answer to its next report, the copy keeping it
until that machine reports doing the same, so no report sent meanwhile
undoes it and nothing expires; a clear covers exactly the alerts the
clearing machine knew of, leaving those an offline machine records after
it. Withdrawing one goes to its machine, found by its number among the
copies, so a sender that moved since finds it, and fails at once, naming
that machine, while it is offline, since a withdrawal that waited could
remove an alert after the user read it. An alert is separate from completion:
telling the user a PR is ready neither ends the assignment nor affects
cleanup. What a finished assignment found travels with its completion
instead: `done --summary` records one line under the rule for an alert sent
up and ends the completion notice with it, and longer findings are files
handed back with it. The sender may withdraw an alert while it is unread;
once read, it cannot, because deleting it would not unsay it, and a
correction does. Each cleanup sweep deletes the user's alerts, read or
unread, recorded longer ago than `cleanup.restore.retention_days` of their
space's repository, whoever sent them, or the global setting's for one about
no space, a chat without a repository or a space whose repository Shoal no
longer knows: an alert left
unread that long no longer concerns the user, and an unread count that only
grows hides the new ones. `hooks.post_agent_exit` exposes a space's
tracked-agent exit alerts to user integrations while it is a ready workspace or a chat that no
removal has, using normal configuration precedence and daemon hook rules with
the agent name, reported code, and process-completion status. Failure notifies
without changing the exit result or marking completion; removal uses its own
hooks. Desktop or push delivery was considered and not adopted.

An alert to a space waits until its agent reads it, which marks it read as
reading marks the user's. An alert sent up is one line, since it may reach
the user's terminal notification; one sent down may span lines, because
instructions to an agent often need several.
Agents read alerts through Shoal commands, and a pending watch
ends for a new alert. Alerts and item activity are pushed rather than polled by the agent,
because agents forget or misuse watches: built-in agent launches register a hook
that delivers both after each tool call and prompt, the only point where a
working agent accepts new context, except a subagent's tool call, whose context
the agent never sees, and replaces a running watch so nothing is
delivered twice. A space's alerts reach its agent once they settle: a run
queued in quick succession, such as the user deciding several approvals, would
otherwise cost a wake or a hook output each, so deliveries hold unread alerts
until none has arrived for a configured window, bounded so a steady stream still
arrives; reading them with `alert list` does not wait. The hook never waits for cleanup or an item change, which can
hold Shoal's item records for minutes: an agent discards the output of a hook
that outlasts its timeout, so the activity waits for a later delivery. Claude Code also runs a background hook, after a turn ends normally
(`Stop`) or on an API error (`StopFailure`), that waits for the next update and
wakes the idle agent with it through `asyncRewake`. The hook
tries a failed wait again instead of exiting, because a hook that ends leaves the
idle agent with no wait until its next turn; waits outlast a daemon restart,
including the time before the next daemon listens and knows the agent's scope. Codex
has no hook that can start a turn, so its tracked wrapper types a fixed prompt
into the place it runs in, where its host types into its places, once Codex is idle with updates, at most once
per idle stretch, and the prompt's hook delivers them. Claude Code's wake hook
does not run after an interrupted turn, so its wrapper prompts it the same way
while it idles after one, and also once no wait for its updates has run for
three minutes, longer than the hook waits between attempts, since an idle agent
whose hook ended for any reason would otherwise wait for the user. The daemon
logs how a wake hook's wait ends without updates, and the hook reports the
reasons only it sees, such as its agent's exit or a signal, so a missed wake
can be traced. Since a missed wake, a needless question or a restore can
still leave an agent idle with work outstanding, a mode's `idle_wake_minutes`
backs these wakes up: an alert wakes an agent idle that long, by the state
its hooks report rather than separate detection, and again after each further
period without backoff, so it reaches the agent through the same delivery as
any update. An agent that legitimately waits says so with `shoal sleep`, until
its next turn or after every turn, which keeps the timer from waking it but no
update, and lets Shoal stop it as above;
a done space is never woken. Only `worker` sets it: the chats wait for the
user by design. Besides that prompt, Shoal writes to an agent's
terminal only the interrupt key `shoal stop --interrupt` sends and the fixed
prompt with which `shoal resume` continues the interrupted turn: the daemon
never does, the prompts carry no forge text, and they are appended to anything
typed but not yet sent. Shoal does not
pre-trust hooks where the agent requires review. Identical unread alerts collapse.
An alert may attach files, which the daemon copies from paths the CLI checked
into the repository directory beside its workspaces: agents get that directory
as a working directory, so a sandboxed agent reads them there without a
prompt, and a copy in the worktree would be an untracked file that holds up
cleanup. A copy outlives its alert, whose path the agent's transcript keeps,
until the workspace is removed, and is named by the alert and the files'
contents, so an identical alert with the same files still collapses.
A workspace being removed takes no alert, since removal decides from the
alerts it has and deletes them with the workspace.
`alert list` reads alerts while `send` sends them down to one space,
a workspace or running chat, or, with `--all`, to every agent in workspaces
and chats, such as to tell them the network works again, or, with `--repo`, to
those of one repository, such as to have them rebase onto a fix: running
agents and those stopped, queued or waiting for their restore, which read it
once they start, since such a message is meant for all work in progress; a
workspace without any agent would hold the alert until removal. An alert
to a chat goes the way the user's own messages to it go, with its files kept
among the chat's. Scoped
processes may read their own space's alerts and send alerts down only to
the agents of the spaces below their own, which they coordinate.

Threads are how a space or the user asks for something whose answer is text,
separate from approvals, whose answer is yes or no. A thread is a
conversation between its opener, the user, a space or Shoal, and its holder,
the user or a space, whoever is asked now, with posts from both sides and
from any other space that reads it, so a question can be discussed before it
is settled, asked downward by the user or a parent of a child, and passed on
to another agent. A thread has a typed
kind, which decides the outcomes it takes: a `question` ends `answered`,
optionally on a choice, a `request`, which asks its holder to do something,
`done` once its opener accepts the work, and every kind `duplicate` of
another thread, `withdrawn` or `closed`, which only Shoal sets. Unknown kinds
and outcomes are refused rather than kept as text. Editors draw a thread with
a state that comes from what Shoal records rather than a field of its own:
`working` while its holder's agent has switched to it, `queued` while its last
post from a side is its opener's, `waiting` while it is the holder's, else its
outcome.

A space opens a thread with its coordinator, else the user, or with a space
below it; the user opens one with a space. A thread lives on the machine of
the space it is numbered in, while its holder may run on any machine of the
hub, which carries the posts between them. An empty thread notifies no one
and keeps nothing, so `thread open` only reserves its number. A space posts
on the side it took part on: the opener's as its opener, the holder's as its
holder, a space above it or an earlier holder, so a coordinator that passed a
question on still answers it. Any other space that reads the thread posts on
neither side, as a participant: its post leaves whose turn it is, offers and
picks no choices, and links the thread for it, so it hears the replies. Each
post reaches the other side's current party as an alert, and its own side's
when someone else posted for it, a participant's both, through the alert
delivery hooks already use, waking an idle agent; the alert is queued in the
transaction that records the post and names the thread as its subject. A post
may offer numbered choices with a recommendation and a reason, or pick a
choice the other side offered by number, never by parsing text, so
`--choose 2` and a post that says "2" cannot be confused. `--wait` waits for
the next post from a side other than the poster's and prints it, marking its
alert read, for agents without Shoal's hooks; a wait an upgrade interrupts
resumes after the same post.

The current party of either side forwards its role, the holder's or, as
when a coordinator takes over its child's question, the opener's: down, to a
space its scope reaches, or up, to its coordinator or the user, never to the
other side's party; the new party reads what came before and receives the
posts from then on. Earlier parties post but no longer forward or resolve.
The forwarder stays linked, since passing a question on rarely ends its
interest in the answer, unless it unlinks. A forward is a post that names the
new party, with its note as text, so the thread's history shows who took
part, and it does not count as an answer. A new opener numbers the thread in
its own space, or the user in the holder's, so the thread's waiting and
lifetime follow it, and the holder and the linkers hear the new number; a
thread does not move between machines, so an opener's role goes only to a
space of the thread's machine. The opener resolves its thread, or the holder
of one Shoal opened, and the user may resolve any; withdrawing takes back the
holder's unread alerts of it, so nobody is asked to decide what no longer
matters, and the other side hears of any other resolution once it has heard
of the thread. `--undo` reopens a resolved thread. A space reads a thread it
took part in, as the space it is numbered in, its opener or holder, now or
earlier,
or one a space its scope reaches took part in, and may link it to receive its
new posts; reading delivers nothing.

A thread waits while it is open and its last post from a side came from the
opener's. A waiting thread keeps the space that opened it from unattended
cleanup, as an agent waits on it, and ranks that space's queued work after
the holder's. The inbox lists the threads whose turn is the user's. `done`
closes every open thread the space is numbered in, opened or holds, on any
machine, since finished work waits on no answer, and tells an opener other
than the space that the holder finished; removing a space does the same.
`done --undo` and a restore reopen the threads closed for that cause,
without Shoal's closing posts, unless something resolved them since.

A post may point at code: a file or its lines in the worktree of a space the
poster's scope reaches, or of the workspace its chat runs in, since a
reference only reads the worktree. References belong to posts rather than threads,
since a request about several places, a reply that points elsewhere and an
answer that says where something should go all attach code mid-conversation;
the opening post's references are the thread's place in code. A worktree's
file is referenced, not copied, so a reference keeps the version of its file
as a Git blob, HEAD then and a snippet of its lines, which shows once the
code is gone. Shoal follows a reference as its code changes, when a read finds
its file differs from the version it last reached, through a diff of that
version against the file now, and records where its lines went, never mapping
from the original: many small diffs keep a range exact, where one diff after
heavy editing can misalign it. There is no background pass. The mapping is a
pure function over two texts in `shoal-protocol`, so an editor's language
server maps a range onto an unsaved buffer as the daemon does once it is
saved. A file renamed whole is found through Git's rename detection; code
moved into another file is not searched for. The repository keeps the
versions under one ref per space, whose commit's tree holds each blob the
space's references keep, since a ref per blob would slow every Git command
that lists refs; the expiry pass rewrites it as references go with their
threads and prunes as checkpoint expiry does.

Shoal records what the agents it runs change, so a thread shows the code its
work changed, a review reads per request and editors read one record,
including after they restart. It attributes changes itself, and the agent
corrects what it cannot tell. An edit tool's hook passes the edit, Claude
Code's Edit, Write and MultiEdit their structured patch, whose hunks are the
agent's, so the user editing the same file meanwhile does not confuse it; a
hunk the file no longer holds where the tool put it is mixed. A tool whose
payload carries no lines, a shell command, Claude Code's NotebookEdit or
Codex's `apply_patch`, is snapshotted before and after: a snapshot is the
tree of the worktree's files Git does not ignore, written through a temporary
index as a checkpoint is, and what changed between the two is the agent's,
or mixed in a file an editor reported the user edited meanwhile. Each space
keeps one snapshot that its next change is diffed against, which an edit
tool's version updates, so what changed between the agent's tool calls, the
user's or a formatter's, is someone else's, and while one command runs, every
change is that command's. Detecting the writing process instead needs root
or an Apple entitlement on macOS, and library injection misses the system
binaries shell commands use. Agents without Shoal's hooks report no tool
calls, so they get no record, and a chat changes no files of its own.

A change is one hunk, numbered in a sequence of its space's own, apart from
`S.N`, so an agent corrects one after its turn; a generated, binary or large
file is one change without hunks that counts in no size and is never asked
about. Its lines follow its code through the same mapping references take, and
stay valid once the agent commits it. Most changes belong to no thread, so
authorship is its own group: `shoal changes` lists, claims and unclaims them,
and the hook after a command lists its mixed changes and asks once, with
nothing waiting on the answer. The agent says which thread its work is for
with `thread switch`, since a request can take several turns and Shoal cannot
infer it from the posts it delivered; a switch lasts until the next or until
the thread is resolved, and a change belongs to at most one thread, an open
one its space takes part in, of any kind. A thread's history shows an entry
per turn and agent between its posts, and a post's alert sums up the changes
since the sender's last post. A post may point at changes as it points at
code, which moves nothing between threads. Changes in a thread go with it;
the others expire as checkpoints do.

An agent posts in its chat with `send <thread> --in-chat`, and its turn's
final message, which the `Stop` hook passes, becomes the post. A user talking
with the agent reads it as the agent wrote it, while a long command line in
the chat is hard to read; a user who is away gets it where they look. Shoal
cannot tell from a host whether the user is there: zmx counts its own
short-lived probes as clients, and an attach from a connection left open
counts while the user is away, so routing posts by attachment left them
unanswered for hours. Agents are therefore not told about attachment, which
only orders overload stops. A prompt the user submits in the chat becomes
the user's reply to the post, carrying its text, since the user answered
there, while one Shoal submits does not.

`send --in-chat` without a thread makes the turn's final message an alert,
for the same reason, through the path every alert takes. Its recipients and
files are resolved and checked when the command runs, so a mistake fails at
once, and the files are copied when the turn ends, as the agent left them.
A turn has one mark, post or alert, so a second one replaces the first and
says so rather than sending both. A prompt the user submits during the turn
drops an alert mark, since the final message then answers the user, and
tells the agent, which may send it again; a prompt in the chat marks the
chat's in-chat alerts to the user read, without recording a reply, since
alerts are one-way.

Threads and approval requests are named `S.N`: the number of the space they
belong to, then the smallest number no thread or request of that space holds.
A thread belongs to its opener's space, or to the space the user or Shoal
opened it with. Both kinds share the space's sequence, so a number names one
item and says which space it belongs to, and counting per space keeps the
numbers short because the user types them. Inside a space a bare thread
number names one of that space, the common case for an agent; elsewhere, and
for approval requests, a bare number or one of the other kind is refused with
the form or command to use rather than guessed. Their records keep a separate
internal key that is never shown. A thread keeps its number until its space
goes, resolved or not, so a late post is refused instead of reaching a later
thread, and a wait follows its thread's space by ID, which a renumbering
leaves alone. A
keep or merge request that no longer applies stays as outdated until the
workspace goes, so its number is never reused and a decision meant for it cannot
approve a newer commit. A proposed issue outlives its space, so its space's
number stays reserved until the last such proposal is decided, keeping the
number the user was told. An agent may
withdraw a pending request it no longer needs, as it may a thread: it stays as
withdrawn on the same terms, except a proposed issue, which would outlive the
workspace and goes as a decided proposal does.

A chat owns attached files as a workspace does, since its agent hands the user
reports and pages too. A removed space keeps every attached file by one rule,
whatever its kind: they stay for the same retention as its checkpoints, since an
agent's restored session may refer to any of them and a chat has no worktree to
clean up, so its files are what remain of it. A copy another machine's file
fetched here goes the same retention after its fetch or last change, by its age
alone: this machine cannot always learn when the source's space goes, and a
fetch happens only while it exists, so an unchanged copy never outlives the
source's retention, while a later fetch of a living space's file makes it again;
an edit keeps the copy that long again, since its changes exist nowhere else.
Disk space cleanup deletes such copies first, since their source machine keeps
them, except one the user changed since its fetch and one a fetch is still
writing. Files are keyed by their owner's ID, which a workspace and a chat never
share, so no kind tells them apart: an owner is gone when neither has the ID,
and a record of its own says how long its files stay, apart from a workspace's
removal record, which a disposable workspace lacks. Each attached file records
who attached it: the user, or the agent whose process tree sent the request, by
both its execution, which pins the run, and the session it reported, which
outlives a restart or resume and names the conversation a client shows the file
in, a chat's agent as a workspace's, since it runs as an execution of its chat;
like scope, this is cooperative. The journal records each file's record as it is
added and deleted, by store triggers so no route of attaching or deleting is
missed, and carries the owner's ID from the record because a removed space has
no row left to supply it; clients showing files follow them without polling. A
file's optional title rides on its argument as `PATH#TITLE`, the form forge
attachments use for alt text, because attach flags repeat and a separate title
flag could not say which file it names.
