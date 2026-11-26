# Threads

This file says what the `thread` and `changes` commands do.

```sh
shoal thread open "<name>" [--kind question|request] [--space S] [--code PATH[:FIRST-LAST]]...
  [--body T | --body-file F] [--severity blocker|should-fix|nit]
shoal send <thread> "<text>" [--ui FILE | --choice C... [--recommend C --reason R]] [--attach FILE]...
  [--code PATH[:FIRST-LAST]]... [--changes N[,N]...] [--wait]
shoal send <thread> --body-file F    # The text from a file
shoal send <thread> --action ID [--value KEY=VALUE]...  # Answer the other side's surface
shoal send <thread> --in-chat        # The turn's final message in the chat is the post
shoal thread forward <thread> [--space S] [--note "<text>"] [--unlink]
shoal thread resolve <thread> <outcome> [--of <thread> | --reason R]
shoal thread resolve <thread> --undo
shoal thread list [--space S] [--state open|resolved|all] [-p]
shoal thread switch [<thread> | --none] [--space S]
shoal thread add <thread> <change>... [--space S]
shoal thread remove <thread> <change>... [--space S]
shoal view thread <thread>
shoal link thread <S.N> related
shoal unlink thread <S.N>
```

[Alerts, threads and changes](../design/coordination.md) in the design says
why threads and changes work as they do.

## Parties and kinds

A thread is a conversation between two sides. Its opener is the user, a space
or Shoal. Its holder, the side asked now, is the user or a space. Both sides
post, and so may other spaces that read the thread.

Each thread has a kind:

- A `question` asks for an answer.
- A `request` asks its holder to do something, such as implement the code it
  points at.
- A `review` thread is a finding of a [local review](reviews.md#local-reviews)
  or a comment of [your own review](reviews.md#your-own-reviews).

## Open a thread

`shoal thread open` records a thread and prints its number. The name is free
text of one line. `--kind` sets the kind. Without it, a thread towards a
workspace you have a pending review of is that review's comment, and any other
thread is a question. `--severity` marks a review comment.

From a space, the thread goes to the space's
[coordinator](create.md#coordinators), or else to you. `--space` sends it to a space the
caller's scope reaches instead. From your terminal, it goes to the space
`--space` names, else the current space, else one you pick.

A new thread is empty unless something gives it an opening post:

- `--body` or `--body-file` gives the post's text.
- `--code` points the post at code. Without a body, the post's text is the
  thread's name.

An empty thread notifies no one, keeps nothing from cleanup and stays out of
the inbox.

## Post

`shoal send` with a thread's number first posts in that thread. Without a
thread number, `--body-file` or one of a post's options, a single argument is
an [alert](alerts.md)'s text.

A post's text is Markdown, up to 8 KiB, and may span lines. `--body-file` reads it from a
file. `--attach` sends files with the post, as an alert sends them. A thread
can hold about 56 KiB, what one response carries. A post that would make it
longer is refused: open a new thread to go on.

### Sides

A space posts on the side it took part on:

- on the opener's side as its opener,
- on the holder's side as its holder, a space above the holder or an earlier
  holder.

Any other space that reads the thread posts as a participant, on neither side.
A participant's post leaves the turn where it was. It offers no surface and
answers none. It also links the thread for that space.

### Who receives a post

Each post sends a `thread_posted` alert to:

- the other side's current party,
- its own side's current party, when someone else posted for it,
- both sides' parties, for a participant's post,
- the spaces that linked the thread.

Nobody receives their own post. An agent's alert carries the post's surface as
text and how to reply. When the alert would pass 8 KiB, it cuts the post's
text and surface short, keeps the commands that answer it, and names
`shoal view thread` for the rest. Yours is one line.

### Surfaces

A post may offer a surface: an interface the other side picks or fills in. A
surface is one [A2UI](https://a2ui.org) 1.0 `createSurface` message. It
carries its components and data model, and never changes once posted.
`--ui FILE` posts one written in JSON, such as:

```json
{"version": "1.0", "createSurface": {"surfaceId": "size",
  "components": [
    {"id": "root", "component": "Column", "children": ["pick", "ok"]},
    {"id": "pick", "component": "ChoicePicker", "label": "Size",
     "options": [{"label": "Small", "value": "s"}, {"label": "Large", "value": "l"}],
     "value": {"@path": "/size"}},
    {"id": "ok", "component": "Button", "child": "ok-label", "variant": "primary",
     "action": {"event": {"name": "order", "context": {"size": {"@path": "/size"}}}}},
    {"id": "ok-label", "component": "Text", "text": "Order"}],
  "dataModel": {"size": ["s"]}}}
```

A surface uses only the components of A2UI's basic catalog. It takes that
catalog's `catalogId` when it names none. A `Text` takes Markdown. An image
names a file sent with `--attach` by its file name: an `Image`'s literal `url`,
or one in Markdown, in a `Text` or in the data model. A `Text` a function
builds, such as `formatString`, takes no bracket outside a whole image in its
own text or the data it joins, since Shoal cannot see what it joins. Code and changes stay
`--code` and `--changes`.

Shoal refuses a surface that:

- takes more than 32 KiB,
- breaks A2UI's schema, or uses a component type the catalog lacks,
- refers to a component it lacks, or has no `root`,
- names a component in two places, or names `root`, so it is no tree,
- has two buttons that dispatch the same action,
- holds a control character, or a newline in a name.

The refusal names what is wrong.

`--choice C`, repeated, is shorthand for the standard surface: one button per
choice, whose actions are numbered from 1. `--recommend`, a choice's text or
number, makes that choice's button the primary one. `--reason` says why,
below the buttons.

### Answers

An action is the event a button dispatches. Its id is the event's `name`. A
button a list repeats for each item is an action for each, `name#N` for item
`N` from 1, or `name#N.M` for item `M` of a list inside item `N`, which reads
its relative bindings in that item's data. An action's name holds no `#`. It
takes a value for each key of its `context` that binds an input's data: a text
field's text, a number field's number or nothing, a checkbox's `true` or
`false`, a slider's number, on one of its steps where it snaps to them, or a
choice picker's option values.

`--action ID` answers the latest surface the other side posted with that
action. `--value KEY=VALUE` gives each value. Repeat a key to select several
options, or give it empty, as `--value tags=`, to select none. The action's label, or its id where the label is blank, is the post's text, cut to fit a post, unless the post gives its own.
Shoal never reads text as an action.

Shoal refuses:

- an action the surface lacks, and a value it does not take, its input
  refuses or the data model has no place for, such as a field of text,
- an answer that fails a check of its button or of an input it takes, which
  a renderer would disable or flag, naming the check's message,
- an answer from a participant, or to your own side's surface,
- an answer to a surface older than that side's latest, naming the current
  post.

The post records the action's whole context, read from the data model with
the values given entered into it, as a renderer sends it. An input's bounds,
such as an end date's earliest, read that data too. Shoal runs `@index`,
`formatString` over data paths, the checks `required`, `length`, `numeric`,
`email` and `regex`, and `and`, `or` and `not` itself, in a context, a check,
a bound and the text it shows. A check it cannot run passes, and one whose
value is missing or of the wrong kind, such as text no number spells, fails. It records any other function call in a context as written, since its
result depends on the renderer, such as its locale or clock, and a bound it
gives bounds nothing. It treats the same way calls nested more than four deep
and a `formatString` whose text would outgrow 8 KiB.

### Surfaces as text

`view thread`, `send --wait`, the inbox and agents' alerts show a surface as
text:

- its text, inputs and layout as indented blocks,
- an image as its alt text and file,
- then, for the latest surface of a side, one line per action. The line gives
  the action's id, label, the values it takes and the command that answers it.

`--json` gives the surface itself.

### Wait for a reply

`--wait` blocks without a time limit. It returns once a side other than the
caller's posts, or the thread is resolved. It prints those posts and marks
their alerts read.

The wait survives daemon restarts. It waits up to two minutes for a daemon
that went away to return and know the caller's scope again. It waits 10
seconds for a daemon that was not running.

### Point at code

`--code` points a post at a file or a directory. It repeats for several.
`:FIRST-LAST` or `:LINE` points at lines of a file. The file is in the worktree
of a space the caller's scope reaches, or of the workspace the caller's chat
runs in. A relative path is read from the current directory.

A reference points at the worktree's file and does not copy it. `--attach`
sends a copy instead. A reference keeps:

- the space whose worktree holds the file,
- HEAD at the time,
- the path and lines,
- the file's version as a Git blob,
- a snippet of the lines, up to 2 KiB.

A reference to a directory, a binary file or a file over 1 MiB keeps only its
path and lines. An editor sends a reference through `shoal-client` with its
buffer's contents, which the reference keeps instead of the file on disk.

The other side receives each reference as `Code: path:first-last` with its
snippet. The reference names the space whose worktree it is in when that is
not the receiver's own.

### Follow code

Shoal follows each reference as its code changes. Reading a thread follows its
references in each ready worktree: `view thread`, `thread list` and
`shoal-client`'s thread requests do. A subscription's snapshot shows where the
references were last followed and moves none.

Following a reference diffs the version it last reached against the file now,
and maps its lines through that diff:

- Lines added or removed above the reference move it.
- Edits inside the reference grow or shrink it to cover what replaced them.
  The reference is then `changed`.
- Once all its lines are deleted, it keeps the point where they were. It is
  then `outdated`, and still shows its snippet.

Shoal finds a file renamed whole where Git's rename detection puts it. It does
not look for code moved into another file, so such a reference is `outdated`.

The repository keeps the versions under `refs/shoal/threads/<space-id>`, one
ref per space. A version stays until its references go with their thread.

`--json` gives each reference's `original` and `latest` place, its `snippet`,
`commit` and `state`. A place is a `path`, `lines` counted from 0 with `end`
excluded, and a `blob`.

### Point at changes

`--changes` takes a list of [change](#agents-changes) numbers and points the
post at those changes. It moves no change between threads. The other side
receives each as `Change N: path:first-last`, with its hunk where its code is
now, cut at 2 KiB.

### Post from the chat

`--in-chat` makes the agent's post in its own conversation, where the user
reads it as the agent wrote it. It works for an agent with Shoal's hooks,
Claude Code or Codex, that Shoal tracks in a workspace or runs as a chat's
agent. Shoal refuses it for other agents.

The command marks the agent's current turn, and a daemon restart keeps the
mark. When the turn ends, the `Stop` hook passes the turn's final message,
which becomes the post. A turn without a final message, as from a hook that
does not pass one, posts that the agent wrote in its chat.

A turn has one mark, a post's or an [alert's](alerts.md). Another `--in-chat`
in the same turn replaces the earlier mark, and the command names it.

A prompt you submit in the agent's chat replies to each such post its thread
waits on. A prompt Shoal submits to wake or continue the agent does not. Shoal
records your prompt as your post. It takes back the post's unread alert, or
tells the agent that held the alert of your reply.

## Forward

`shoal thread forward` gives the caller's role in a thread, its current opener
or holder, to another party:

- with `--space`, to a space the caller's scope reaches,
- without it, up to the caller's coordinator, or else you.

A role never goes to the other side's party. From your terminal, it goes only
to a space. The opener's role goes only to a space on the thread's machine.

The forward is a post that names the new party, with `--note` as its text. The
new party receives a `thread_forwarded` alert with what the thread says so far,
and its posts from then on. A new opener numbers the thread in its own space,
or you in the holder's. The holder and the spaces that linked the thread hear
the new number.

The forwarding space stays linked to the thread and receives its later posts,
unless `--unlink`. Earlier openers and holders still post on their side, but
neither forward nor resolve.

## Resolve

`shoal thread resolve` settles a thread with an outcome its kind takes:

- `answered`, for a question.
- `done`, for a request whose work its opener accepted.
- a review finding's outcome, which [local reviews](reviews.md#local-reviews)
  list.
- `duplicate --of <thread>`, for any kind.
- `withdrawn`, for any kind, with an optional `--reason`.

Only Shoal sets `closed`, with its cause, `completion` or `removal`.

The opener resolves its thread, the holder resolves one Shoal opened, and you
resolve any thread. `--undo` reopens a resolved thread.

Withdrawing a thread takes back the holder's unread alerts of it. Any other
outcome sends the other side a `thread_resolved` alert, once that side has
heard of the thread.

## Numbers

Threads and [approval requests](approvals.md#approval-requests) are numbered
`S.N`, so `3.2` names one item of either kind:

- `S` is the [number of the space](workspaces.md#space-numbers) the thread
  belongs to: its opener's, or the one you or Shoal opened it with.
- `N` is the smallest number that no thread or request of that space holds.

Inside a space, a bare number names a thread of that space. Elsewhere, and for
an approval request, Shoal refuses a number without its space, or one that
names the other kind, and names the form or command to use.

A thread keeps its number, resolved or not, until its space goes. The thread is
kept on the machine of that space, while its holder may run on another machine
of the [hub](hub.md).

`--json` gives `space_number`, `item` and the `number` text.

## Read and link

A space reads a thread it opened, holds or held, and one that a space its
scope reaches did. You read every thread. Reading delivers nothing.

A space that reads a thread may `link thread <S.N> related` to it. The
thread's new posts then reach that space as alerts, until `unlink thread`.

### List and view

`shoal thread list` lists the threads a space opened, holds, held or linked.
It lists your own space's by default, or every thread from your terminal.
`--state` filters them, `open` by default. The list holds the newest threads
that fit one response and counts the older ones it leaves out.

On a terminal, `thread list` opens a picker. Choosing a thread shows its
posts, as `shoal view thread` does. When its turn is yours and the other
side's latest surface offers actions, a picker offers them under that post:

- The recommended action comes first, where the picker starts.
- Picking an action asks for each value it takes, in the order its inputs
  show, picking among an input's options or none of them. A typed value
  offers the one the surface holds, which Enter keeps and a lone `-` clears.
- Picking the reply in words asks for the reply.
- Ctrl-C leaves.

Without such a surface, a `Reply` prompt takes the reply as written.

With `-p`, `--json` or redirected input or output, listing and viewing print
records without prompting.

## Waiting threads

A thread is waiting while it is open and its last post came from its opener's
side. A waiting thread keeps the space that opened it from
[unattended cleanup](removal.md#automatic-cleanup). The
[inbox](workspaces.md#inbox) lists the threads whose turn is yours.

## Completion and removal

`shoal done` closes every open thread the space opened or holds, and every
thread opened with it. The opener gets Shoal's post that the holder finished,
unless the opener is that space. Removing the space does the same.

`done --undo` and [restoring](removal.md#restore-a-removed-space) the space
reopen the threads closed for that cause, without Shoal's posts. A thread
something else resolved since stays resolved.

## Threads as JSON

`--json` gives a thread's:

- `kind`, `name`, `opener` and `holder`,
- `posts`, each with its `side`, `sender`, `space`, `text`, `ui`, the surface
  it offers, `answer`, with the `post` it answers, the `action` and its
  `values`, `files`, `code`, `changes`, `in_chat` and the holder it
  `forwarded_to`,
- `resolution`, with its `outcome`, its data, `by`, `by_space` and
  `resolved_at`,
- `working`, whether its holder's agent has switched to it,
- `changes`.

## Agents' changes

```sh
shoal changes [--thread <thread> | --space S]   # Latest changes, numbered, with their threads
shoal changes claim <change>... [--thread <thread>] [--space S]
shoal changes unclaim <change>... [--space S]
shoal diff --thread <thread> [--since <post>]
```

### Recording

Shoal records what a workspace's agent changes in its worktree, when the agent
has Shoal's hooks: Claude Code or Codex. It leaves out files Git ignores.
Agents without the hooks, and chats, record no changes.

Claude Code's Edit, Write and MultiEdit pass their patch. Its hunks are the
agent's. A hunk is mixed where the file no longer holds the lines where the
tool put them.

Shoal snapshots the worktree before and after a command, Claude Code's
NotebookEdit and Codex's `apply_patch`. Each snapshot delays the tool by the
time Git takes to hash the worktree's unsaved files.

- What changed between the two snapshots is the agent's.
- It is mixed in a file an editor reported you edited or saved while the tool
  ran.
- What changed since the agent's previous tool call is someone else's.

### A change

A change is one hunk of a file. It records:

- its path,
- its lines in the version it made, which the space's ref keeps as it keeps
  [references](#follow-code)' versions,
- the lines it added and removed,
- who made it: `agent`, `mixed` or `others`,
- the agent's turn.

A generated, binary or large file is one change with its size and no hunks.
A generated file is one the repository marks `linguist-generated` in
`.gitattributes`, and a large one is over 1 MiB. Such a change counts in no
size and is never asked about. It shows as `generated`, `binary` or `large`.

Changes are numbered in their space's own sequence, apart from threads and
approval requests, and keep their number. A bare number names a change of
your own space, or of `--space`. `S.N` names change `N` of the space
numbered `S`.

A change's lines follow its code as a reference's do, also once it is
committed.

### Claim mixed changes

After a command leaves mixed changes, the hook's context lists them. It asks
you to `claim` those you made and `unclaim` the others. Nothing waits on the
answer, and a change left unanswered stays `mixed`.

- `claim` makes changes the agent's. `--thread` puts them in that thread.
- `unclaim` makes changes someone else's, in no thread.

`shoal changes` lists your space's latest 200 changes, or with `--thread`
that thread's. Each shows who made it, where it is now, its size and its
thread.

### Changes for a thread

`shoal thread switch <thread>` sends your changes from then on to an open
thread your space takes part in, as its opener's space or its holder, of any
kind. The switch lasts until the next switch or until the thread is resolved.
Your own changes and mixed ones go to the thread, and someone else's to none.

- `--none` sends your changes to no thread.
- Without a thread or `--none`, the command prints the current thread, or on a
  terminal picks an open one.

A thread delivered to your agent reminds it to switch before working on it.
Editors show a thread `working` while its holder's agent has switched to it.

A change belongs to at most one thread. `thread add` puts changes into a
thread and out of any other. `thread remove` takes them out.

### A thread's changes

Between its posts, a thread's history shows one entry per turn and agent, with
the files it changed for the thread and their size. `view thread` numbers the
posts. The entries take at most 8 KiB, which the posts leave room for. Once
they would take more, the oldest sum up their files without naming them, and
then merge.

A post's alert sums up the changes made for the thread since the sender's
previous post, such as `3 files changed, +40 −12`. It names the
`shoal diff --thread <thread> --since <post>` that prints them:

- each hunk under its file, where its code is now, without context,
- each file without hunks, by its kind and size.

The diff is cut at 32 KiB.

`--json` gives each change's `number`, `space`, `author`, `kind`, `made_at`,
`turn`, `execution`, `thread`, `original` and `latest` places, `state`,
`added`, `removed` and `size`. A diff adds `before`, `removed_lines` and
`added_lines`.

An editor reads the same changes through `shoal-client`, and reports the files
you edit.

### Expiry

A change in a thread goes with the thread. A change in no thread, that no post
points at, expires after
[`[cleanup.restore] retention_days`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#cleanup),
as checkpoints do.
