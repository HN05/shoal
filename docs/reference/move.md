# Moving spaces

This file says what `shoal move` and `shoal restore --from` do.
[Moves](../design/moves.md) gives the reasons.

```sh
shoal move 7 -o topic.shoal                    # remove workspace 7 and write it to a file
shoal move 5 -o plan.shoal                     # end the chat 5 and write it to a file
shoal restore --from topic.shoal               # continue it in the repository with the same origin
shoal move 7 -o - | ssh other shoal restore --from -   # agents continue there
shoal move 5 --ssh other                       # the same, once ssh reaches Shoal there
shoal move 7 --machine server                  # move workspace 7 to the joined machine server
shoal move --repo app --all -o moving          # end every space of app, a file per tree
shoal move 7 8 -o moving                       # remove both, a file each unless 8 is below 7
shoal move 7 --no-recursive -o topic.shoal     # leave the spaces below 7 here, one level up
shoal move 7 --keep -o copy.shoal              # keep workspace 7 running and write a copy of it
shoal move 7 --ssh other --keep                # keep workspace 7 running and copy it there
shoal move 7                                   # ask for the destination
shoal restore --from moving                    # continue every space written to moving
```

## Choose a destination

`shoal move [space]...` takes one destination:

- `-o <file>` writes the spaces to a [file](#write-to-a-file).
- `--machine <machine>` moves them to a [joined machine](#move-to-a-joined-machine).
- `--ssh <host>` moves them to a machine [`ssh` reaches](#move-over-ssh).

Without a destination, on a terminal, it asks:

1. It picks the spaces when none are named.
2. It asks for the destination's kind: `Joined machine`, `SSH host` or `File`.
   `Joined machine` comes first while another joined machine is online.
   `--no-resume` leaves out `File`, and `--shoal` leaves only `SSH host`.
3. For a joined machine, it picks one of the other joined machines online, by
   name. The offline ones are listed above them, each with when the hub last
   saw it. For a host or file, it asks for the host or path.

It refuses spaces, a repository or a `--checkpoint` it cannot take before it
asks. Without a terminal, a move without a destination is refused, and the
error names each flag.

## How a move runs

The daemon runs the move, so the move finishes even after the terminal that
started it closes.

The daemon checks the spaces before it ends any. A move that would end spaces
besides those named, or release holds, ends nothing at first. The command asks,
unless given `--yes`, listing each space with its kind and agent. It then starts
the move again for what the user agreed to.

The command follows the move and prints a line for each space as it goes. With
`--json`, it prints what the move did with each space, under `moved` and
`failed`. An interrupt cancels the move before its next space. A second
interrupt stops following and leaves the move running.

### A move that lost its worker

Shoal records each space's move before its agents stop. When the move's worker
goes, as when the machine restarts during the move, Shoal finishes the move from
how far it got:

- A space whose destination has its package ends here.
- A space none of whose package left moves again.
- A space whose package `-o -` had not yet written stays here, and its agents'
  restores are queued.
- A space that another machine, or a reader of what `-o -` wrote, may have taken
  stays here with its agents stopped, as when a connection drops.

An alert tells how each space finished. Until Shoal finishes such a move,
another move of that space is refused with the error code `move_in_progress`.

## What a move takes

A space carries the spaces below it, workspaces and chats alike, and the chats
in the worktree of each workspace among them. They go in its package parents
first, and the same removal ends them. Each workspace has a checkpoint of its
own.

A workspace ends here as [`shoal rm`](removal.md#remove-a-space) removes it. Its
package holds its removal record and checkpoint. A chat's package holds its
removal record with its agent's session. The package's manifest names each
record's `kind`, `workspace` or `chat`.

Each checkpoint travels as a Git bundle without the commits of the default
remote's default branch. A checkpoint that branch already contains travels
without a bundle.

Ignored files, leases, permits, approvals, grants and holds stay behind. Setup
rebuilds what the workspace needs.

A space whose agent reported no session moves with a warning. Its agent starts
as a new one where the space arrives.

### Spaces that stay

`--no-recursive` leaves the spaces below here, with any destination.
`--recursive` takes them, the default except with `--machine`. A space below
that is named too goes in the package that carries the space above it. The
chats in the worktree of a carried workspace always go with it.

Each space that stays keeps running, with the spaces below it. On a move to a
joined machine, it stays below the moved space. Otherwise it moves up one level,
below the moved space's parent, or to the top level when that parent cannot
take it.

A workspace that stays keeps its branch and the branch it builds on, which the
removal keeps. It loses only its link to the moved workspace. Requests that the
moved spaces decided for the spaces that stay pass to the nearest space above
them with an agent, or to the user.

## Write to a file

`shoal move [space] -o <file>` writes a workspace or chat to a new file. It ends
the space here once the file holds it. The space is the one named, else the
current workspace, else one a picker offers among workspaces and chats.

The daemon stops the space's agents, saving their sessions, writes the file and
ends the space. With `-o -`, the command copies the package to standard output,
then ends the space. Standard output must not be a terminal.

The export refuses, before it ends anything:

- when the file exists,
- when the daemon cannot create a file in its directory within 30 seconds,
- when `-o -` would stream from a place that closes with the space.

It also refuses, listing each failing space, when a space it takes:

- is a workspace whose uncommitted files a checkpoint cannot keep,
- is held by another lifecycle operation,
- holds the file,
- is, for `-o -`, one the terminal closes with or the chat the command runs in.

A workspace whose HEAD or unsaved files changed after its file was written
stays here too. The report says so and names the `shoal rm` that removes it
here.

### Several spaces

`-o` names a directory for several spaces:

- `shoal move <space>... -o <directory>`, or the spaces a picker selects when
  `-o` names an existing directory;
- `shoal move --repo <repository> --all -o <directory>`, for every workspace
  and running chat of the repository.

The move writes each space to `<id>.shoal` in the directory, which it creates
when missing. A file is named after the space's ID, since names repeat. A space
below another selected space goes in that space's file. So the directory gets a
file for each space that no other selected space carries.

The move checks every space as a single export does. It also checks that no
workspace is in another lifecycle operation. When one space cannot be exported,
it lists each refusal and ends nothing. It asks once before ending the spaces,
then exports each one and lists the files it wrote. A failure there leaves the
other spaces to export. The command then exits 1 and names each failed space.

### Removed spaces

A space already removed, as by cleanup, exports from its removal record. The
export takes the records of the spaces its removal took. Every record stays
here, and nothing is removed again. Each refusal that still applies to a
removed space holds.

With `--no-recursive`, a removed space exports with its own record only. The
records of the spaces its removal took stay here, and restoring one opens it at
the top level.

### Spaces on another joined machine

A space another joined machine runs, or whose removal record it keeps, exports
to a file here too. That machine packs the space and keeps the package until
this machine's daemon has read it through the hub and written the file or the
command's standard output. That machine then ends the space, or for `--keep`
deletes the copy's package. It also deletes a package that no machine reads
within 30 minutes. A removed space's record stays on that machine.

With several spaces, each machine's spaces go after this machine's. With spaces
of more than one machine, each other machine first checks its own without
ending any. The one question lists what each machine would end.

## Copies

`--keep` leaves the spaces and the spaces below them running, and moves copies
of them instead. A copy is:

- for a workspace, a new checkpoint, as `shoal checkpoint` takes one;
- for the named workspace with `--checkpoint N`, an earlier checkpoint, as
  `shoal restore <workspace> --checkpoint` lists them;
- for a chat, a copy with its session.

`--keep` refuses a removed space. `--checkpoint` takes one space only.

With several spaces, a directory or `--all`, `--keep -o` writes a copy of each
space that no other selected space carries to `<id>.shoal` in the directory. It
reports each space it cannot copy and copies the rest.

### Opening a copy

A copy opens under an ID of its own, below the copy of the space it was below.

A workspace copy opens as a new workspace on the nearest free `-2`, `-3`, ...
variant of its branch. It links to the original's issues, but not its PRs or CI
runs. A workspace copy that builds on its parent's branch builds on the copy's
branch.

A chat copy opens as a new chat.

`shoal resume` starts a copy's agents from forks of their sessions, as
[`shoal start --fork`](create.md#fork-the-parents-agent) does. An agent without
a session starts as a new agent, told the space is a copy. Forks get session IDs
of their own, so the original conversations never change.

A copy of an earlier checkpoint warns at export and at import that the agents'
conversations describe work its files lack. Its agents are told so too.

`restore --from` refuses `--keep-sessions` for a copy.

## Move to a joined machine

`shoal move [space]... --machine <machine>` moves spaces to another machine
joined to the same hub. `--machine` takes the machine's name or the ID
`shoal hub ls` shows. The move can start on any joined machine. The machine
that runs the spaces runs it.

Both machines must be online. A move to this machine or to an offline one is
refused before anything stops. The refusal of an offline machine names when the
hub last saw it.

The named spaces move, with the chats in the worktree of each moving workspace.
`--recursive` takes the spaces below too. The moved space keeps its ID and
number there, and the spaces that stay keep running below it.

The package goes over the hub's links to that machine's daemon. That daemon
continues it as [`shoal restore --from -`](#import-a-package) does, and clones a
repository it lacks. It resumes the agents unless `--no-resume`, telling each
where its space is now.

### The claim

The destination's claim of the space at the hub commits the move:

- When the destination took none of the package, the spaces stay here, and
  their agents' restores are queued.
- Once its claim stands, the spaces are the destination's and end here. That
  holds when setting them up there fails too. The move reports the failure, and
  `shoal restore <id>` retries it there.

When the destination cannot say how its import went, as when the link drops,
the move asks the hub whether it claimed the space. While the hub cannot be
asked, the agents stay stopped here and the move asks again every 30 seconds.
Once the hub answers, a machine that did not claim the space no longer can.

`--keep` copies the spaces there as new spaces, without a claim.

### Removed spaces

A removed space's record moves to the joined machine too. That machine records
the space as removed under its ID and claims it at the hub, so
`shoal restore <id>` opens it there. This machine forgets the record once the
claim stands. It keeps the record when the claim is refused. When the
destination could not report the arrival, this machine refuses to restore the
space until the hub settles it.

### Agents moving spaces

A space's agent moves every space its scope reaches to a joined machine,
chats included, with no permission key. It does so from another joined machine
too. It ends a space the user holds as its `user_holds` permission allows. It
moves the record of a removed space it may restore. A copy of its own space,
which opens beside it, takes its `admin` permission.

A move that would stop the agent asking for it, as of its own space or of the
workspace its chat runs in, starts once that agent's turn ends. The agent then
continues on the other machine from its session. A move that leaves the space
here tells the agent why.

## Move over SSH

`shoal move [space]... --ssh <host>`, or with `--repo <repository> --all`,
moves each space with
`ssh -o BatchMode=yes <host> shoal --json restore --from -`.

Before it ends anything, the move runs `ssh <host> shoal repo ls`. When Shoal
does not answer there, nothing ends.

The daemon runs `ssh`, which never prompts. The host must take a key that works
without a prompt, as Git's fetches need. The move reads a host alias from the
SSH config of the machine that runs it. On a machine joined to a hub, the move
of another machine's space runs on that machine, as the user's, so that
machine's `ssh` must reach the host. Only the user moves another machine's
space over SSH.

`--shoal <path>` names the program there when the remote shell's `PATH` lacks
it. `--no-resume` leaves the agents stopped there, a chat's as a workspace's.

The move checks the spaces as a [batch export](#several-spaces) does and asks
once before ending them. Each space then moves on its own, after the spaces
below it:

1. Shoal stops the agents of the space and of the chats that go with it, saving
   their sessions.
2. It exports the space while the space stays here.
3. It imports the space there.
4. Once that import succeeds, it ends the space here through the export's
   removal.

### When it fails

When the export fails, `ssh` cannot run, or the import fails with
`not_imported`, the space stays here. Its stopped agents' restores are queued,
and the report gives the other machine's error.

When the import fails otherwise, as when setup there fails or the connection
drops, the space stays here with its agents stopped, since the other machine
may have it. The report names the `shoal rm` that removes the space here once it
arrived there, and the `shoal resume` that continues it here otherwise.

A workspace with a space below it that stayed is not exported and stays here.

The import can succeed while the space cannot end here. A workspace's HEAD or
unsaved files may have changed since its export, its own `hooks.pre_remove`
included, or its `hooks.pre_remove` may fail. The space then stays here too. The
report says it now exists on both machines and names the `shoal rm` that removes
it here.

### Copies over SSH

`--keep`, with `--checkpoint N` for one workspace, copies the spaces as
[`--keep -o`](#copies) does. It imports each copy there as a new space and
leaves the space running here. It asks nothing. It reports a space it cannot
copy or import there, and copies the rest.

## Import a package

`shoal restore --from <file>` hands a package to the daemon. `-` reads standard
input, which must not be a terminal. The daemon reads the package whole and
refuses one that another Shoal version wrote.

### Where a space goes

The import continues the package's space:

- in the registered repository with the exported repository's origin,
- or in the repository `--repo` names,
- or, for a chat without a repository, in `root_dir`.

A space the package carries goes to the repository with its own origin. When it
is of the package's repository, it goes where the package's space goes.

When no registered repository has an origin the import needs, the import
registers that origin as a clone, as `shoal start <url>` does.

### Several packages

`--from` takes several files, or a directory, whose `.shoal` files it reads in
name order. The import continues each one and goes on after a failure. It lists
every space it continued. It exits 1 and names each file it could not import or
whose `hooks.post_setup` failed.

### How a space opens

The import records each space as removed, then restores it as
[`shoal restore`](removal.md#restore-a-removed-space) does, under its ID. A
workspace's record gets its checkpoint once the repository has the commit the
bundle builds on, fetching the default branch when it lacks it. When the
restore fails, `shoal restore <id>` retries it.

The spaces a package carries open after its space the same way: parents first,
each below its parent, and each workspace set up before the spaces below it. A
space that cannot open fails the import, which goes on with the rest and names
each failure. So `shoal move --ssh` keeps such a space here. Its record there,
where the import made one, stays for `shoal restore <id>`.

An imported chat opens as `shoal restore` reopens a
[removed chat](removal.md#restore-a-removed-chat). It opens in the worktree it
ran in where the importing machine has that workspace ready, such as one that
moved there before. Otherwise it opens in the repository's checkout with a
warning, or in `root_dir` without a repository.

The import queues the restores of the agents the export stopped, as
`shoal restore` does, under the importing machine's agent limits. With
`--no-resume`, the agents of every imported space stay stopped, and the report
names the `shoal resume` that resumes them.

### Sessions

The package carries the files of each session the agents reported, such as
Claude Code's and Codex's transcripts. The import puts them where the agent
looks for them. It keeps a file already there unless the imported one continues
it. So a session continues by its ID on any machine.

The export leaves out a session whose files it could not read, with a warning.
That session continues only on the machine that exported it. `--keep-sessions`
keeps every session, such as one whose files were copied by hand.
`--new-sessions` keeps none.

The session files stay with the removal record until the restore that opens the
space puts them in place, as the space's own configuration describes its
agents. They wait in the repository directory's `.sessions`, or in `root_dir`'s
for a chat without a repository. An export of that record carries them again.
Files the restore cannot put in place, as in a development build without
`SHOAL_DEV_USER_FILES`, stay for the space's next removal record.

Every reopen, by import or `shoal resume`, trusts the workspace's path for the
agents that resume there. `shoal resume` starts an agent without a kept session
as `shoal start --in` starts a new one, instead of running its
`resume_command`. The agent is told the workspace moved.

### Refusals

An import is refused, before it records anything, when a space with the ID of
the package's space, or of a space it carries, is already here. `shoal rm`
removes that space here. Or `shoal move --keep` where the package was written
writes a copy, which opens under an ID of its own.

An import that fails before it records anything of the space, such as one whose
clone fails or whose package ends early, reports the error code `not_imported`.

## Who moves spaces

A scoped process moves a space to a file or over SSH, and imports a package, as
its space's `admin` [permission](modes.md#permitted-actions) allows. It moves only the
spaces its scope reaches. It imports only into its own repository, so its import
clones none. A move to a joined machine follows
[its own rule](#agents-moving-spaces). `shoal move` refuses a
[master session](chats.md#master-session).
