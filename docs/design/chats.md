# Chats

This file covers chats, the spaces without a worktree of their own, including
the chat without a repository. [Chats](../reference/chats.md) says what each
command does.

## What chats get

A chat gets every feature that coordinates or communicates, such as alerts,
threads, links and requests to the forge, including starting CI runs. Those
features take the same code path for both kinds of space.

A chat gets nothing that runs code on this machine, such as leases,
`shoal run` and demos. It gets nothing that needs a worktree or a branch of its
own, such as PRs.

## A conversation, not an assignment

A chat is a conversation with an agent about a repository. It is not an
assignment, so it runs in the registered checkout instead of in a workspace.
Coordinating work is one thing a chat may do.

A chat starts at once from the default branch, which the background sync keeps
current. An offline or diverged checkout thus still gets its chat.

A repository may have several chats, so each start makes a new one.

### Instructions

A chat's instructions describe the discussion only. They come from the `chat`
rule, which repositories extend like the other rules, as
[Modes](modes.md) describes.

A chat waits for the user to speak first unless it opens on a task. An agent
whose tool takes instructions apart from the first message gets them so.
Another agent gets them as its first message, which starts a turn. That message
therefore asks only for a short reply, or carries the task.

### Launcher

A chat's launcher runs where only the checkout's path has a value. Shoal
refuses a launcher that uses another workspace placeholder, rather than pass it
the placeholder's literal text.

## Scope

Every chat is scoped, as a workspace's processes are. Its agent carries a scope
token that covers the chat and the spaces below it. The chat keeps the token
across stops and restores. Only the user's own terminal thus acts unscoped.
[Scope](permissions.md#scope) says what a scope allows.

The agent follows the rules of a workspace's agent, without what needs a
worktree. It opens workspaces and chats only below its chat and in its
repository. It never takes over a space outside its scope.

### Proposals

A chat's agent proposes issues where its permissions say so, as a workspace's
agent does. The proposal records no branch or commit, since a chat has none.
Approval opens the issue from the chat's checkout and records it as the chat's.

A chat never proposes a PR, since it has no branch.

## The chat's agent

A chat's agent runs under the execution wrapper as a tracked execution of its
chat. It takes the path a workspace's agent takes. Launches, stops, restores,
wakes, exit reports, turn state and restarts are thus one code path for both
kinds of space. The chat runs while that execution is recorded.

The agent's mode's weight decides its capacity as for a workspace's agent, as
[Agent queue](queue.md) describes. This includes the restore of an idle agent,
which waits for an update. A user who wants it sooner attaches to the chat.

Only the agent's clean exit differs by kind. It notifies no one, since the user
is in that conversation.

### Turn state

A chat's agent runs the turn-state hooks of a workspace's agent. It runs no
worktree hooks or lifecycle permission rules, since those need a worktree.
`agent-state` and `turn-failed` act on the space they run in, and record the
chat's state as a workspace's.

The execution's session files keep the session, so the chat can move. The
agent's state ends with its execution, as a workspace agent's does. Keep-awake
reads it as it reads a workspace agent's.

### Stop and restore

A stop that keeps the wrapper restores the agent in place. A stop that ends the
wrapper leaves its stop record for `shoal resume`. When the agent is to come
back on its own, the record goes to the agent queue instead, which restores the
chat in the place it ran in.

A stopped chat keeps its record, and with it what
[Stop and resume](../reference/chats.md#stop-and-resume) lists, until a restore
runs its agent again.

`shoal resume` resolves each target to a space in one lookup over the shared
namespace. Its picker, `--all` and a list of names thus act on chats as on
workspaces. An interrupted turn continues in a chat as in a workspace, and
`shoal restore --discard-agent` discards a chat's stopped agent as a
workspace's.

### Events and messages

A chat's starts, renames and removals are journal events. Its agent's starts and
stops are logged as a workspace agent's.

A chat's messages, including the user's request to rename it, wait and reach
its agent as a workspace's do. A removed space, of either kind, gives the user
the alerts that its unread alerts stand for.

## Name and place

A chat starts under its default name without asking the user for one. Its agent
learns what the work is and renames the chat with `shoal rename`.

A chat's name is a label, as a workspace's is, and may repeat. Its files are
kept under its ID.

Renaming changes the record, not the zmx session, since zmx names a session
only when it creates it.

### Recording the chat first

The daemon records the chat before the tab or session that hosts it opens. The
place thus carries the chat's name, and a detach from its zmx session names its
number.

The chat's record names the session it runs in. `shoal attach` thus finds the
chat there, not by a session name that zmx may have suffixed. The chat labels
the session with its ID, so the chat's removal closes a session Shoal created,
and no workspace's removal does. [Terminal hosts](terminals.md) says how labels
carry ownership.

### Happy chats

A Happy chat has no terminal to focus. It starts detached, as a workspace's
Happy session does, so the user continues it in the Happy app.

## Removal

Only removal ends a chat: `shoal rm`, or space cleanup. Removing the chat first
removes the spaces below it, through the path that removes any space's
children.

Cleanup treats a chat as a workspace whose worktree is always clean, pushed and
merged. It counts the chat's running agent as use, a queued restore as a queued
agent, and its saved stopped work as a workspace's. Merged cleanup counts the
chat's idle time from when its agent stopped. [Completion and
cleanup](cleanup.md) says how cleanup decides.

## Coordination

A workspace a chat opens is its child in the one tree of spaces, which
[Spaces](spaces.md) describes. Shoal thus knows which chat coordinates which
workers. A workspace created below another workspace stays there.

Reopening the workspace outside a chat, for its issue or a picked branch,
returns it to the user. A start of its own agent leaves it where it is, with
`--in` or with the `--branch` it has.

A chat opens workspaces only in its own repository.

### Events of the spaces below

A stopped chat passes the events of the spaces below it on up, to the user at
the top. A chat that waits for its automatic restore keeps them.

A chat passes a thread up to the user, who attends it.

### Approval requests

The approval requests of the spaces below a chat come to it when it is the
nearest space above them with an agent. It decides them as a parent workspace
does, by its own permissions or grants, as [Spaces](spaces.md) describes.

Once the chat's agent stops, or the chat is removed, the requests it has not
decided go to the user, since no one attends them in the chat.

## Chat in a workspace's worktree

A chat may run in a ready workspace's worktree instead of the checkout. It
debugs, audits or reviews the workspace's live work, committed and uncommitted.
It uses the commands that read a branch, rather than reading another worktree
by path.

### One editing agent

A workspace has one editing agent, its own. A chat in its worktree reads and
does not edit. Its instructions say so. Nothing stops it from editing files,
since Shoal does not yet restrict the filesystem, as
[Overview](overview.md#filesystem-restrictions-decided-policy-not-implemented)
says.

The daemon keeps the workspace's own agent one. It refuses to queue a launch in
an existing workspace while that workspace's agent runs, waits in the queue or
is stopped awaiting `shoal resume`. The CLI, coordinators and apps thus get one
answer. A start on a terminal asks instead of refusing, since the user is there
to choose.

A last session gives way, since its agent ended on its own. A stopped or asleep
agent gives way only to a start with `--replace`. That start discards it
through the operation `shoal restore --discard-agent` runs, so replacing needs
the right to discard. A running agent never gives way, so a start never cuts
off a live turn.

### Not the workspace's child

A chat in a worktree stays a chat, with its own identity, scope, temporary
directory and messages. It is not the workspace's child: a child's threads and
approvals go to its parent's agent, while the user attends a chat. The Shoal
commands it runs thus never act as the workspace's agent.

Its scope reads the workspace, as well as the chat and the spaces below it. It
refuses any request that changes the workspace.

The chat's record names the workspace. A restore therefore runs the chat there
again. Commands that take the workspace from the directory act on the
workspace, and those that act on chats keep acting on the chat.

### The workspace's lifetime

A running chat in a workspace counts as use, so no unattended removal takes the
workspace from under it. A chat Shoal saved counts as the workspace's saved
stopped work. It keeps the workspace from idle cleanup but not from merged
cleanup.

Removing the workspace first removes its chats, through the chat removal path.
A workspace one of them opened that cannot go thus keeps the chat, which keeps
the workspace.

Nothing moves a session between directories. A chat about another workspace is
a new chat.

## Master session

A chat may belong to no repository. A mode with `repository = false`, such as
the built-in `orchestrate`, starts it in `root_dir` with the global config
alone. Such a chat is a master session, whose scope reaches every space on the
machine.

A master session lets the user hand coordination to a tracked, scoped agent. It
starts work below itself in any repository. It receives what its subtree
reports, as any parent does. It acts on other spaces as a coordinator acts on
the spaces below it. What the scope keeps from every agent stays the user's.

### Repositories

Every registered repository counts as a master session's own. Its forge actions
take the repository they act in, with the permissions that repository's config
resolves. Its proposals belong to that repository.

A master session links nothing, since its links would name no one repository.

In a repository that is not registered, it opens an issue only directly, since a
proposal belongs to a registered repository.
