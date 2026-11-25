# Attached files

This file says what the `files` commands do.

```sh
shoal files [space]          # Pick among a workspace's or chat's files, or list them
shoal files [space] --print  # List them on a terminal too
shoal files [space] --path   # Only the paths of Shoal's copies
shoal done --attach report.md    # Hand back a file with the completion
shoal files add notes.md [--space S]      # Attach files without a message
shoal files add 'chart.html#Latency by region'  # Attach a file with a title
shoal files rm [FILE...] [--space S]      # Delete a space's files, or those with these names
shoal files rm --all                      # Delete the files of every idle workspace and removed space
```

## Spaces and copies

A space's attached files belong to a workspace or a [chat](chats.md), here or
on another joined machine. Shoal keeps a copy of each in the repository
directory's `.attachments`. A chat without a repository keeps them in the
`root_dir` it started in. A file of another machine's space stays on that
machine.

The commands name a space by its number or ID, and a removed space by its ID.
Run from a chat's agent outside any workspace, `shoal files`, `add` and `rm`
act on its chat.

## Pick a file

On a terminal, `shoal files` opens a picker over the space's files, with a
`+ Add file` row. Its keys act on the selected file:

- Enter prints a text file.
- Ctrl-Y prints the path of its copy.
- Ctrl-O opens it in the application the desktop chooses and prints its path.
- Ctrl-D deletes its copy, as `rm` does.
- Ctrl-A, or Enter on the add row, asks for a file to add.

Workspace processes cannot open files.

Ctrl-O first fetches a file another machine keeps. The fetched copy goes into
`.fetched` beside the workspaces of its repository, or into `root_dir` where
this machine lacks the repository. A later fetch of the same file replaces it.
A fetched copy stays for the
[retention period](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#cleanup)
after its fetch or last change, and goes with its repository.

## List files

`shoal files` lists the files instead of picking with `-p`/`--print`, `--json`
or `--path`, and when its output is not a terminal. Each row shows the file's
name, title, size, source, when it came and where its copy is: a path on this
machine, or the machine that keeps it.

The source says how the file came:

- `message`: sent down with `shoal send --attach`.
- `alert`: sent up with `shoal send --attach`.
- `thread`: sent with a post in a [thread](threads.md).
- `added`: attached with `shoal files add`.
- `result`: handed back with [`shoal done --attach`](completion.md).
- `handoff`: a copy of an agent's session that a
  [handoff to another tool](chats.md#convert-a-chat-or-a-workspace) made.

`--path` lists only the paths of the copies this machine keeps.

`--json` gives each file as a record:

- `space` names the space it is attached to: its `id`, its `kind`, and its
  `name` when the file was attached, or its last once removed.
- `source`, `bytes`, and `title` when the file has one.
- `path`, or `machine` for a copy another machine keeps.
- `expires_at` for a removed space's file.
- `attached_by` is `{"kind": "user"}` for a file attached from outside any
  agent. A file an agent attached gives `{"kind": "agent"}` with its name as
  `agent`, its `execution` and, once the agent has reported it, its `session`,
  a workspace's agent or a chat's.
- `session` describes a handoff's copy: the `agent`, `session` and
  `integration` it copies, the `file`'s path in that tool's session
  directory, and in `transcript` whether that file is the conversation itself.

## Attach files

Every way of attaching takes a file as `PATH` or `PATH#TITLE`, split at the
first `#`. The title says what the file is, in one line of at most 128
characters. Clients show the name of a file without one.

`shoal files add` copies files of at most 100 MiB into the space's files. It
tells none of the space's agents. Adding the same file again keeps its copy,
and keeps its title unless the new call gives one.

## Delete files

`shoal files rm` deletes a space's copies before their retention ends: all of
them, or only the copies of the named files. It asks first while an agent or
command runs in the workspace, or while the chat runs.

`--all` deletes the files of every workspace and chat where nothing runs, and
of every removed space. It asks first; noninteractive and JSON calls confirm
with `--yes`.

Sending a file again makes a new copy of a deleted one.

## Retention

A space's files outlive it. They stay for the
[retention period](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#cleanup)
after its removal, as its [checkpoints](removal.md#checkpoints-and-restore)
do. Until then, `shoal files <id>` lists them with when they go, and `rm`
deletes them. Both work on another joined machine too, since the hub knows
where each removed space's files are. `shoal restore` gives the files back to
the space it reopens.

When space runs low, [disk space](load.md) cleanup may delete a removed
space's files, and fetched copies nobody changed since their fetch, before
their retention ends.

## Scope

Scoped agents list, add and delete the files of their own workspace, of the
spaces below it and of the removed spaces that were below one of those. They
read a file posted in a thread they read, wherever it is kept. They cannot use
`--all` or reach a chat's files.
