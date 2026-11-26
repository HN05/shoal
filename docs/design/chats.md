# Chats

This file covers chats, the spaces without a worktree of their own, including
the chat without a repository.

A chat gets every feature that coordinates or communicates, including
requests to the forge such as starting CI runs. It gets nothing that runs code
on this machine, such as leases, `shoal run` and demos, and nothing that needs
a worktree or branch of its own. Features of the first kind take the same code
path for both kinds of space.

A chat is a conversation with an agent about a repository, not an assignment,
so it runs in the registered checkout instead of in a workspace: coordinating
work is one thing a chat may do. Every chat is scoped, as a workspace's
processes are: its agent carries a scope token, kept with the chat across
stops and restores, that covers the chat and the spaces below it, so only the
user's own terminal acts unscoped. The agent follows the workspace-agent rules
without what needs a worktree: it opens workspaces and chats only below its
chat and in its repository, never takes over one outside it, and renames only
its own chat. It proposes issues where its permissions say so, as a workspace's agent
does, recording no branch or commit, since it has none; approval opens the
issue from the chat's checkout and records it as the chat's. It never proposes
a PR, since it has no branch. It starts at once from the default branch the
background sync keeps current, so an offline or diverged checkout still gets its
chat. Its
instructions describe the discussion only, from the `chat` rule that
repositories can extend like the other rules.
A chat's agent
runs under the execution wrapper as a tracked execution of its chat, through
the path a workspace's agent takes, so launches, stops, restores, wakes, exit
reports, turn state and restarts are one code path, and the chat runs while
that execution is recorded. Its agent's weight decides its capacity as for a
workspace's agent, including a restore between turns waiting for an update,
which the user who wants it sooner gets by attaching; only its clean exit
differs by kind, notifying no one, since the user is in that conversation. A
stop that keeps the wrapper restores the agent in place; one that ends it
leaves its stop record for `shoal resume`, or for the agent queue when the
agent is to come back on its own, which starts the chat's restore in the
place the chat ran in. Once the chat's agent ends, the requests the chat
decided go to the user, since no one attends them there. A stopped chat keeps
its record, and with it its name, links, files, messages, threads,
workspaces and temporary directory, until a restore runs its agent again. Only
removal ends a chat: `shoal rm`, or space cleanup, which treats a chat as a
workspace whose worktree is always clean, pushed and merged, its running agent
as use, a queued restore as a queued agent and its saved stopped work as a
workspace's, and takes the idle time of merged cleanup from when its agent
stopped. `shoal resume` resolves each target to a space in one lookup over the
shared namespace, so its picker, `--all` and a list of names act on chats as on
workspaces, an interrupted turn continues in a chat as in a workspace, and
`shoal restore --discard-agent` discards a chat's stopped agent as a workspace's.
Starts, renames and removals are journal events, and its agent's starts and
stops are logged as a workspace agent's. A workspace a chat opens is its child in the one tree of
spaces, unless it was created below a workspace, where it stays; reopening
it outside a chat for its issue or a picked branch returns it to the user, so
Shoal knows which chat coordinates which workers, while a start of its own
agent, with `--in` or the `--branch` it has, leaves it where it is. Removing the chat first removes the spaces below it through the
removal path of any space's children, and a stopped chat passes their events
on up, to the user at the top, unless it waits for its automatic restore. A
chat opens workspaces only in its own repository.
A chat may run in a ready workspace's worktree instead of the checkout, to
debug, audit or review that workspace's live work, committed and uncommitted,
with the commands that read a branch, rather than reading another worktree by
path. A workspace has one editing agent, its own: a chat in it reads and does
not edit, which its instructions say and nothing enforces until modes decide
what an agent may do. The daemon keeps the workspace's own agent one: it
refuses to queue a launch in an existing workspace while that workspace's
agent runs, waits in the queue or is stopped awaiting `shoal resume`, so the
CLI, coordinators and apps get one answer; a last session gives way, since its
agent ended on its own. A stopped or asleep agent gives way only to a start
with `--replace`, which discards it through the same operation as
`shoal restore --discard-agent`, so replacing needs the right to discard; a
running one never does, so a start never cuts off a live turn. A start on a
terminal asks instead of refusing, since the user is there to choose. It stays a chat, with its own identity, scope,
temporary directory and messages, and is not the workspace's child, since a
child's threads and approvals go to its parent's agent while the user attends
a chat; so Shoal commands it runs never act as the workspace's agent. Its scope
reads the workspace as well as the chat and the spaces below it, and refuses
any request that changes the workspace. Its
record names the workspace, so a restore runs it there again, commands that
take the workspace from the directory act on it, and those that act on chats
keep acting on the chat. A running chat in a workspace is use, so no unattended
removal takes the workspace from under it, and one Shoal saved is the
workspace's saved stopped work, which keeps it from idle cleanup but not merged
cleanup. Removing the workspace removes its chats first through the chat
removal path, so a workspace one of them opened that cannot go keeps the chat,
which keeps the workspace. Nothing moves a session between directories: a chat
about another workspace is a new chat.
A chat's messages, including the user's request to rename it, wait and reach
its agent as a workspace's do; a removed space, of either kind, gives the user
the alerts its unread alerts stand for.
A chat passes a thread up to the user, who attends it. The approval
requests of the spaces below it come to it when it is the nearest space above
them with an agent, and it approves them by its own permissions or grants, as
a parent workspace does, and otherwise denies or passes them up; those it
has not decided go to the user when its agent stops or it is removed, as with
any deciding space. A repository may have several chats, so each call starts a new
one, placed as other launches are. A chat has a name as a workspace has, a label that may repeat; its files are
kept under its ID. A chat starts under its default name without asking
the user for one: its agent learns what the work is and renames the chat with
`shoal rename`. Renaming changes the record, not the zmx session, which zmx
names only at creation. A place that
hosts a space, a workspace or a chat, takes a new label only while it still
shows the label Shoal gave it, since another label is the user's: a chat's its
old name, a workspace's the label its previous title rendered. A place labelled
before its workspace's template, issue or branch changed keeps that stale label
rather than risk overwriting the user's. The daemon records the chat
before the tab or session that hosts it opens, which carries its name, and a detach
from its zmx session names its number; the chat's record names
the session it runs in, so `shoal attach` finds it there, not by a session name
zmx may have suffixed, and the chat labels the session with its ID, so the
chat's removal closes a session Shoal created and no workspace's end does. A chat waits for the user to speak first unless it opens on a
task: an agent that takes instructions apart from the first message gets them
so, and another gets them as its first message, which starts a turn and so asks
only for a short reply or carries the task.
A chat's launcher runs where only the checkout's path has a value, so one that
uses another workspace placeholder is refused rather than given the literal text.
A Happy chat has no terminal to focus: like a workspace Happy session it starts
detached, as Happy's daemon would start it, so the user continues it in the
Happy app.
A chat's agent runs the turn-state hooks a workspace's agent runs, without the
worktree hooks and lifecycle permission rules, which need a worktree. The
`agent-state` and `turn-failed` commands act on the space they run in and record
the chat's state as a workspace's, and the execution's session files keep the
session so the chat can move. Its state ends with its agent's execution, as a
workspace agent's does; keep-awake reads it as a workspace agent's.

A chat may also belong to no repository: a mode with `repository = false`,
such as the built-in `orchestrate`, starts it in `root_dir`, with the global
config alone, as a master session whose scope reaches every space on the
machine. It starts work below itself in any repository, receives what its
subtree reports as any parent does, and acts on other spaces as a coordinator
acts below it, while what the scope keeps from every agent stays the user's,
so the user can hand coordination to a tracked, scoped agent. Every registered
repository counts as its own, so its forge actions take the repository they
act in and their permissions as that repository's config resolves them, and
its proposals belong to that repository; it links nothing, since its links would name no one
repository.
In a repository that is not registered, it opens an issue only directly,
since a proposal belongs to a registered repository.
