# Threads

This file says what the `thread` and `changes` commands do.

```sh
shoal thread open "<name>" [--kind question|request] [--space S] [--code PATH[:FIRST-LAST]]...
  [--body T | --body-file F] [--severity blocker|should-fix|nit]
shoal send <thread> "<text>" [--choice C]... [--recommend R --reason R] [--attach FILE]...
  [--code PATH[:FIRST-LAST]]... [--changes N[,N]...] [--wait]
shoal send <thread> --body-file F    # The text from a file
shoal send <thread> --choose N       # Pick a choice the other side offered
shoal send <thread> --in-chat        # The turn's final message in the chat is the post
shoal thread forward <thread> [--space S] [--note "<text>"] [--unlink]
shoal thread resolve <thread> <outcome> [--choose N | --of <thread> | --reason R]
shoal thread resolve <thread> --undo
shoal thread list [--space S] [--state open|resolved|all] [-p]
shoal thread switch [<thread> | --none] [--space S]
shoal thread add <thread> <change>... [--space S]
shoal thread remove <thread> <change>... [--space S]
shoal view thread <thread>
shoal link thread <S.N> related
shoal unlink thread <S.N>
```

A thread is a conversation between its opener, the user, a space or Shoal, and
its holder, the user or a space, whoever is asked now, with posts from both
sides and from other spaces that read it. `shoal thread open` records a thread with a free-text name of one
line and a kind, `question` or `request`, and prints its number, empty unless
`--body`, or `--body-file`, or `--code` gives it an opening post: the body,
else the name, pointing at that code. A question asks for an answer, and a request asks its holder to
do something, such as implement the code it points at. Without `--kind`, a
thread towards a workspace you have a pending review of is a `review` thread,
[its comment](reviews.md#your-own-reviews), and otherwise a question; Shoal opens one for
each finding of a [local review](reviews.md#local-reviews). From a space it
goes to the space's [coordinator](create.md), else to you, or with
`--space` to a space the caller's scope reaches; from your terminal it goes to
the space `--space` names, else the current or a picked one. An empty thread
notifies no one, keeps nothing from cleanup and stays out of the inbox.

`shoal send` led by a thread's number posts there: the text, up to 8 KiB and
spanning lines, or one read with `--body-file`, and optionally numbered
choices, each one line, with the one it recommends and why, and files sent with
`--attach` as alerts send them. `--choose N` picks the choice of that number
the other side offered last, its text the post's unless given; text is never
read as a choice number. A single argument without `--body-file` or a post's
option is an [alert](alerts.md)'s text. A space posts on the side it took part
on: the opener's as its opener, the holder's as its holder, a space above it or
an earlier holder. Any other space that reads the thread posts as a
participant, on neither side: its post leaves whose turn it is, offers and
picks no choices, and links the thread for that space. Each post reaches the
other side's current party as a `thread_posted` alert, an agent's with the
choices and how to reply and yours in one line, its own side's current party
when someone else posted for it, both sides' for a participant's post, and the
spaces that linked the thread; nobody receives their own post. `--wait` blocks
without a time limit until a post comes from a side other than the caller's,
or the thread is resolved, prints those posts, and marks their alerts read; it survives daemon restarts, waiting up to two minutes for a
daemon that went away to return and know the caller's scope again, or 10
seconds for one that was not running. A post that would make the thread longer
than one response carries, about 56 KiB, is refused: open a new thread to go on.

`--code`, repeatable, points a post at a file or directory, or with
`:FIRST-LAST`, or `:LINE`, at lines of a file, in the worktree of a space the
caller's scope reaches, or of the workspace the caller's chat runs in,
which the reference names; a relative path is read
from the current directory. The worktree's file is referenced, not copied:
`--attach` sends a copy. A reference keeps the space, HEAD then, the path and
lines, the file's version as a Git blob, and a snippet of its lines of up to
2 KiB; one to a directory, a binary file or a file over 1 MiB keeps its path
and lines only. Through `shoal-client`, an editor sends a reference with its
buffer's contents, which it keeps instead of the file on disk. The other side
receives each as `Code: path:first-last` with the snippet, naming the space
whose worktree it is in when that is not its own.

Shoal follows each reference as its code changes. Reading a thread, with
`view thread`, `thread list` or `shoal-client`'s requests for threads, but
not a subscription's snapshot, which shows where its references were last
followed, compares the file of each
reference in a ready worktree with the version the reference was last followed
to, and on a change maps its lines through a diff of that version against the
file now: lines added or removed above them move them, edits inside them grow
or shrink them to cover what replaced them, which leaves the reference
`changed`, and once all of them are deleted it keeps the point where they
were, `outdated`, still showing its snippet. A file renamed whole is found
where Git's rename detection puts it; code moved into another file is not
looked for, so the reference is `outdated`. `--json` gives each reference's
`original` place and `latest`, each a `path`, `lines` counted from 0 with
`end` excluded, and `blob`, its `snippet`, `commit` and `state`. The
repository keeps the versions under `refs/shoal/threads/<space-id>`, one ref
per space, until the references go with their thread.

`--changes`, a list of [change](#agents-changes) numbers, points a post at
those changes, which moves nothing between threads; the other side receives
each as `Change N: path:first-last` with its hunk where its code is then, cut
at a snippet's 2 KiB.

`--in-chat` lets an agent with Shoal's hooks, Claude Code or Codex, that Shoal
tracks in a workspace or runs as a chat's, post in its conversation, where the
post reads as the agent wrote it; other agents are refused. It marks the
agent's current turn, which a daemon restart keeps, and when the turn ends the
`Stop` hook passes the turn's final message, which becomes the post. A turn
has one mark, a post's or an [alert's](alerts.md): another `--in-chat` in it
replaces the earlier one, which the command names. A prompt
you submit in the agent's chat, rather than one Shoal submits to wake or
continue it, is your reply to each such post its thread waits on: Shoal
records it as your post and takes back the post's unread alert, or tells the
agent that held it of your reply. A turn without a final message, as from a
hook that does not pass one, posts that the agent wrote in its chat.

`shoal thread forward` gives the role the caller has in a thread, as its
current opener or holder, to a space its scope reaches, or without `--space` up
to the caller's coordinator, or you; never to the other side's party, and from
your terminal only to a space. The forward and its `--note` are a post naming
the new party, which receives a `thread_forwarded` alert with what the thread
says so far and its posts from then on. A new opener numbers the thread in its
space, or for you the holder's, and the holder and the spaces that linked it
hear the new number; the opener's role goes only to a space on the thread's
machine. Earlier openers and holders post on their side but neither forward nor
resolve. The forwarding space stays linked to the thread, so it
receives its later posts too, unless `--unlink`.

`shoal thread resolve` settles a thread with an outcome its kind takes:
`answered`, for a question, with `--choose` naming the choice it settled on;
`done`, for a request whose work its opener accepted; for a review's finding
`fixed`, `false-positive` or `wont-fix` with `--reason`, or `deferred` with
`--issue`; `duplicate --of <thread>`; or `withdrawn`, with an optional `--reason`. Shoal
sets `closed`, with its cause, `completion` or `removal`. The opener resolves
its thread, the holder one Shoal opened, and you any thread; `--undo` reopens
a resolved one. Withdrawing takes back the holder's unread alerts of the
thread, and otherwise, once the other side heard of the thread, it receives a
`thread_resolved` alert.

Threads and [approval requests](approvals.md#resource-approvals) are numbered `S.N`: the
[number of the space](workspaces.md) a thread belongs to, its opener's or the
one you or Shoal opened it with, then the smallest number no thread or request
of that space holds, so `3.2` names one item of either kind. Inside a space a
bare number names a thread of that space; elsewhere, and for an approval
request, a number without its space, or one that names the other kind, is
refused with the form or command to use. `--json` gives `space_number`, `item`
and the `number` text. A thread keeps its number, resolved or not, until its
space goes. It is kept on the machine of that space, while its holder may run
on another machine of the hub.

A space reads a thread it opened, holds or held, or that a space its scope
reaches did, and you read every thread; reading delivers nothing. Any space
that reads a thread may `link thread <S.N> related` it: its new posts then
reach that space as alerts, until `unlink thread`. `shoal thread list` lists
the threads a space opened, holds, held or linked, your own space's by
default, or every thread from your terminal, in `--state` (`open` by default),
the newest that fit one response, counting the older ones it leaves out. On a
terminal it opens a picker, and choosing a thread, or `shoal view thread`,
shows its posts; when its turn is yours, a `Reply` prompt follows, where Enter
takes the latest post's recommendation shown in brackets and anything typed is
the reply as written, and Ctrl-C leaves. With `-p`, JSON or redirected input or
output, listing and viewing print records without prompting.

A thread is waiting while it is open and its last post came from its opener's
side. A waiting thread keeps the space that opened it from
[unattended cleanup](removal.md#automatic-cleanup), and the [inbox](workspaces.md#inbox) lists the threads whose
turn is yours. `shoal done` closes every open thread the space opened or holds,
or that was opened with it, and its opener, unless that is the space, gets
Shoal's post that the holder finished; removing the space does the same. `done
--undo` and [restoring](removal.md#checkpoints-and-restore) the space reopen the threads closed
for that cause that nothing resolved otherwise since, without Shoal's posts.
`--json` gives a thread's `kind`, `name`, `opener`, `holder`, its `posts`, each
with its `side`, `sender`, `space`, `text`, `choices`, `recommendation`,
`reason`, `choice`, `files`, `code`, `changes`, `in_chat` and the holder it
`forwarded_to`, its `resolution` with its `outcome`, its data, `by`,
`by_space` and `resolved_at`, whether it is `working`, and its `changes`.

## Agents' changes

```sh
shoal changes [--thread <thread> | --space S]   # Latest changes, numbered, with their threads
shoal changes claim <change>... [--thread <thread>] [--space S]
shoal changes unclaim <change>... [--space S]
shoal diff --thread <thread> [--since <post>]
```

Shoal records what a workspace's agent with its hooks, Claude Code or Codex,
changes in the worktree, leaving out files Git ignores. Claude Code's Edit, Write and MultiEdit pass their
patch, whose hunks are the agent's, mixed where the file no longer holds the
lines where the tool put them. A command, Claude Code's NotebookEdit and
Codex's `apply_patch` are snapshotted before and after, which delays each by
the time Git takes to hash the worktree's unsaved files: what changed between
is the agent's, or mixed in a file an editor reported you edited or saved
while it ran, and what changed since the agent's previous tool call is
someone else's. Agents without the hooks, and chats, record no changes.

A change is one hunk of a file: its path, its lines in the version it made,
which the space's ref keeps as it keeps [references](threads.md)' versions, the
lines it added and removed, who made it (`agent`, `mixed` or `others`) and
the agent's turn. A file the repository marks `linguist-generated` in
`.gitattributes`, a binary file and one over 1 MiB are one change each, with
their size and no hunks: they count in no size, are never asked about and
show as `generated`, `binary` or `large`. Changes are numbered in their
space's own sequence, apart from threads and approval requests, and keep their
number; a bare number names one of your own space, or of `--space`, and
`S.N` change `N` of the space numbered `S`. A change's lines follow its code
as a reference's do, also once it is committed.

After a command leaves mixed changes, the hook's context lists them and asks
you to `claim` those you made and `unclaim` the others; nothing waits on the
answer, and an unanswered one stays `mixed`. `claim` makes changes the
agent's, with `--thread` for that thread; `unclaim` makes them someone
else's, for no thread. `shoal changes` lists your space's latest 200 changes,
or with `--thread` that thread's, each with who made it, where it is now, its
size and its thread.

`shoal thread switch <thread>` makes your changes from then on go to an open
thread your space takes part in, as its opener's space or its holder, of any
kind, until the next switch or until the thread is resolved; `--none` sends
them to none, and without either it prints the current one, or on a terminal
picks an open one. Your own and mixed changes go to it, someone else's to
none. A change belongs to at most one thread: `thread add` puts changes into
one, out of any other, and `thread remove` takes them out. A thread delivered
to your agent reminds it to switch before working on it, and editors show a
thread `working` while its holder's agent has switched to it.

A thread's history shows, between its posts, one entry per turn and agent with
the files it changed for the thread and their size, and `view thread` numbers
its posts. Its entries take at most 8 KiB, which posts leave room for: once
they would take more, the oldest sum up their files without naming them, then
merge. A post's alert sums up the changes made for the thread since the
sender's previous post, such as `3 files changed, +40 −12`, and names the
`shoal diff --thread <thread> --since <post>` that prints them: each hunk
under its file where its code is now, without context and cut at 32 KiB, and
each file without hunks by its kind and size. `--json` gives each change's `number`, `space`,
`author`, `kind`, `made_at`, `turn`, `execution`, `thread`, `original` and
`latest` places, `state`, `added`, `removed` and `size`, and a diff's
`before`, `removed_lines` and `added_lines`. Through `shoal-client`, an editor
reads the same changes and reports the files you edit.

A change in a thread goes with it; one in no thread that no post points at
expires after [`[cleanup.restore] retention_days`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#cleanup), as checkpoints do.
