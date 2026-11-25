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

`shoal files` lists the files attached to a space, a workspace or
[chat](chats.md), here or on another joined machine, or on a terminal opens a
picker over them with a `+ Add file` row: Enter prints a text file, Ctrl-Y its
copy's path, Ctrl-O opens it in the application the desktop chooses and prints
its path, Ctrl-D deletes that copy as `rm` does, and Ctrl-A, or Enter on the
add row, asks for a file to add. Ctrl-O first fetches a file another machine
keeps into `.fetched` beside the workspaces of its repository, where this
machine has the repository, else in `root_dir`, replacing the copy an earlier
Ctrl-O fetched. A fetched copy stays for `cleanup.restore.retention_days`
after its fetch or last change, the repository's or else the global
setting, and goes with its repository; one nobody changed since its fetch
is the first thing [disk space](load.md) cleanup deletes. Workspace processes
cannot open files.
`-p`/`--print`, like `--json`, `--path` or output that is not a
terminal, lists them: each one's name, title, size,
how it came (`message` for a file sent down with `shoal send --attach`,
`added` for one from `shoal files add`, `result` for one handed back with
`shoal done --attach`, `alert` and `thread` for one sent with
`shoal send --attach` up and with a post in a thread, `handoff` for a copy
of an agent's session that a [handoff to another
tool](chats.md#convert-a-chat-or-a-workspace) made), when, and the path of Shoal's copy in the repository
directory's `.attachments`, or for a chat without a repository that of the
`root_dir` it started in, or for another machine's space that machine;
`--path` lists only the paths of the copies this machine keeps.
A space's files outlive it: they stay for `cleanup.restore.retention_days`
after its removal, like a workspace's
[checkpoints](removal.md#checkpoints-and-restore), and `shoal files <id>` still lists
them, with when they go, and deletes them with `rm`, on another joined
machine too, which the hub tells where they are kept. `shoal restore` gives
them back to the space it reopens, where they were. Disk space cleanup
deletes them sooner when nothing else frees enough space. A space is named by
the workspace's or chat's number or ID, a removed one's by ID; run from a chat's agent outside any workspace, `shoal files`, `add`
and `rm` act on its chat.
`--json` gives each as a record with `space`, what it is attached to, by its
`id`, its `name` then or its last once removed, and its `kind`, `source`, `path`, or `machine` for a copy another machine keeps, `bytes`, `title` when it has one, and
`attached_by`:
`{"kind": "user"}` for a file attached from outside any agent,
or `{"kind": "agent"}` with the agent's name as `agent`, a workspace's or a
chat's, its `execution` and, once the agent has reported it, its `session`.
A handoff's copy adds `session`, the `agent`, `session` and `integration`
it copies, the `file`'s path in that tool's session directory and whether it
is the conversation itself in `transcript`.
Every way of attaching takes a file as `PATH` or
`PATH#TITLE`, splitting at the first `#`; the title, a single line of at most
128 characters, says what the file is, and clients show the name where there is
none. `shoal files add` copies files of at most 100 MiB
there without telling the workspace's agents; adding the same file again keeps
its copy, and its title unless a new one is given. `shoal files rm` deletes
copies sooner: all of a space's, or with file names only the copies of those
files. It asks first when an agent or command runs in
the workspace, or the chat runs. `--all` deletes the files of every workspace
and chat where none runs, and of every removed space, after asking; noninteractive and JSON calls confirm with `--yes`. Sending files
again copies any deleted copy anew. Scoped agents may list, add and delete the
files of their own workspace and the spaces below it, and the files of a
removed space that was below one of those, and read a file posted in a
thread they read wherever it is kept, but not use `--all` or reach a chat's
files.
