# Moves

This file covers moving spaces to another machine, or through a file, as
packages. [Moving spaces](../reference/move.md) says what `shoal move` and
`shoal restore --from` do; this file gives the reasons.

## A removal there, a restore here

A move ends a space on one machine and continues it on another. The package
holds the space's removal record, its agents' sessions and the sessions'
files. The import records the space as removed, then restores it as
`shoal restore` does. So importing is a form of `shoal restore`, which
`--from` names.

The restore keeps the space's ID. It opens the space below its parent where
that parent is on the importing machine.

Workspaces and chats move through one path. Export, copy, move and restore
differ by kind only in a workspace's worktree step: its checkpoint and its
branch.

### Why a move ends the space

A move ends the space where it was, rather than copying it. That keeps one
workspace per branch, so two agents never push the branch or watch its PRs.
The source keeps the removal record, so the space stays restorable there when
the transfer fails.

Moving a chat ends it too, stopping its agent. A session that kept growing here
would fork from the one that continues there.

A space already removed exports from its removal record, a chat's as a
workspace's. The record stays, so the space stays restorable here.

## What a package carries

### The spaces below

Removing a space takes the spaces below it, whatever their kind. So its
package carries them. One walk down `space_parents` gives them parents first,
with the chats in each carried worktree. Each then opens below its parent
again.

`--no-recursive` leaves the spaces below here instead, so they keep running as
they were. With `-o` or `--ssh`, each moves up one level through the tree's
parent link. The removal keeps the branch a staying workspace builds on.

`--machine` leaves the spaces below by default. The moved space keeps its ID on
the other machine, so the hub routes their events there. They need no new
parent. The removal here would take them along, so they are detached while it
runs. Its record holds them until they are linked again. A daemon restart
during the removal finishes linking them.

### A workspace

The export writes the workspace's removal record and a Git bundle of its
checkpoint. It checks the destination before anything ends. The import
records both as a removal in the matching repository, the ref before the
records, then runs the normal restore.

The bundle leaves out the commits of the default remote's default branch. That
branch only advances, so the importer fetches it when it lacks the commit the
bundle builds on.

An import finds the repository by its origin. It registers an origin no
registered repository has as a clone, as `shoal start --machine` does. So a
package needs nothing set up before it arrives.

### The format

A package names the Shoal version that wrote it. Another version refuses it,
since the shapes it holds change freely between versions.

## Copies

A copy leaves the space running. It keeps one workspace per branch by
becoming new work under an ID of its own.

A workspace's copy is one of its checkpoints. The import opens it as a new
workspace on a branch of its own. It links the original's issues, but not its
PRs or CI runs, which belong to the original's branch.

Every copied agent, a chat's included, starts from a fork of its session under
a new ID. So the original conversation never changes, even on the same
machine.

A copy of an earlier checkpoint carries sessions newer than its files. The
export, the import and the forked agents are each told so.

## One command for every destination

`shoal move` is one command whose destination is a flag: `-o`, `--ssh` or
`--machine`. The space selection, `--keep`, `--checkpoint` and `--recursive`
mean the same for every destination. A new destination is one more flag.

Each destination sets whether the spaces below go by default. Every
destination accepts both `--recursive` and `--no-recursive`.

Without a destination, a terminal asks for one. A joined machine comes first
while another one is online, since the hub needs nothing set up between the
machines. Without a terminal, the move is refused. So an archive reaches
standard output only through `-o -`.

A move always names its machine. A placement policy chooses where a new space
starts, never a move's destination. Deciding when and where to move work
belongs to a caller outside the core.

## The move operation

A move runs in the daemon as an operation the command follows, as creation,
`pr open` and merge do. So a move finishes even after the terminal that
started it closes. Every destination takes this one path, and only the
delivery differs.

The move's worker checks the spaces as a batch does. For each space, the
daemon stops its agents, saving their sessions, and writes the package. The
worker delivers the package. The daemon ends the spaces here once their
destination has it.

### Consent

The worker checks every space before any ends. Ending spaces besides the one
named, or releasing holds, needs the user. The worker then ends nothing and
asks. The command, or an app, starts the move again with consent to what the
question listed.

### Several spaces

Exporting every workspace of a repository checks each one before it removes
any. So one workspace that cannot leave refuses the set, and all stay. A
failure once removals begin leaves the exported spaces moved and the rest in
place, each reported.

Copying several spaces needs no such check, since a copy removes nothing. A
space that cannot be copied is reported, and the rest are copied.

### A move of the caller's own space

A move that would stop the agent that asked for it waits for that agent's turn
to end, as a conversion does. Stopping the agent mid-turn would cut off the
command that asked. The daemon records the waiting move, so a restart keeps it.

### Recovery

The daemon records each space's move before any agent stops. The record holds
the destination and the stopped work its spaces saved. Once the package is
written, the record also holds its checkpoints and the move's phase. The worker
holds a lock on the space while it moves the space.

A recorded move whose lock is free lost its worker. The daemon then finishes
the move from the phase its record reached, in a worker of its own, instead of
guessing:

- A package that left ends the spaces here.
- A package of which nothing left moves again.
- An import that may have happened leaves the agents stopped on both machines,
  as a dropped connection does. On a joined machine, the hub settles it
  instead.

A worker that outlives a daemon restart finishes its move itself. It loses only
the reports its followers would have read.

## Delivery

### To a file

`-o <file>` has the daemon write the package. `-o -` hands the package to the
command instead. The command holds the move's lock and ends the spaces once it
has copied the package.

`-o` writes where the command runs. When another joined machine has the space,
or keeps its removal record, that machine only packs it. It holds the package
for the command's machine to read through the hub.

Export and import move work through any byte stream. The user runs both
halves and chooses their order.

### Over SSH

`--ssh` pipes the package into `shoal --json restore --from -` over `ssh`. So
the other side stays a plain command. `ssh` runs in batch mode, so the host
must take a key that works without a prompt, as Git's fetches need.

The move first checks that Shoal answers there. It ends a space here only once
the import there succeeded, since a chat keeps no record once it ends:

- When the import failed before it recorded anything, the agents' restores are
  queued here, and no file is kept.
- When the import may have recorded the space, the agents stay stopped here. So
  the space's agents never run on both machines.

A space can fail to end here after its import, such as a workspace that changed
since its export. Shoal checks again once the space's removal hooks have run.
The space then stays, existing on both machines, rather than losing the newer
work.

### To a joined machine

`--machine` streams the package to the other machine's daemon, over a duplex
stream of the hub's links. That daemon pipes it into
`shoal internal import-moved`, the same restore path. So that side too is a
plain command.

The destination's claim of the space at the hub commits the move. The
destination records the space as removed, then claims it. A refused claim
forgets the record. Once the claim stands, the record stays there for
`shoal restore` whatever fails after it. [One machine per
space](hub.md#one-machine-per-space) says how the hub keeps that claim.

Each recorded move holds a ticket, which its destination's claims carry. A
source may fail to learn whether the destination claimed the space. It then
asks the hub to settle the move. The hub answers from its claims and owners.
When the destination has not claimed the space, the hub refuses that ticket's
later claims. So an import still running there cannot take the space after its
source restored it.

While the hub cannot be asked, the source keeps the agents stopped and asks
again instead of guessing. So the agents never run on both machines.

#### Removed spaces

A removed space's record moves through the same delivery and claim. It goes in
one daemon request without a recorded move, since no agent runs and nothing
ends here. The destination's daemon records the space as removed without
opening it.

The source forgets the record once the claim stands, or once the hub says it
does. A source that stops before it learns keeps the record, as an export to a
file does.

## The import

The daemon reads a package itself, from a file or from the bytes a client
streams on its connection. It checks the package whole before it records
anything. So every import takes one path, whether a terminal, another daemon
or the hub delivers it.

The daemon records every space the package carries along with the package's
own space, and opens its own. The caller opens the rest in order, setting each
workspace up before the chats in its worktree.

An import resumes the agents of every space it opens, a chat's as a
workspace's, unless `--no-resume` leaves them stopped. It does so over SSH as
from a file, so a move behaves the same whatever carries it.

### Chats

The import starts a chat through the chat launch, in its mode, with the
agent's resume arguments. The chat opens in the worktree of the workspace it
ran in, where that workspace is on the importing machine and ready. Workspaces
keep their IDs across moves, so a chat finds a workspace that moved before it.
Otherwise the chat opens in the repository's checkout, or in the importing
machine's `root_dir` without a repository.

The chat takes a free name, since its own may be taken there. It starts in the
background when no terminal attends the import, as queued agents start.

## Sessions

Agent conversations are the agent tools' private files, which Shoal never
reads or changes. The daemon reports the sessions a space's agents reported.
The client finds each session's files by ID where its tool keeps them, under
the client's environment or one the repository's agents ran with.

The export copies each reported session's files unread. The import puts them at
the same path under the importing agent's session directory. It replaces a file
there only with one that continues it, as a session that went away and came
back does. A development build leaves placing them to the installed Shoal.

A session whose files cannot be read is left out with a warning, and its agent
starts over. A space whose agent reported no session moves with a warning,
whatever its kind. Its agent starts as a new one there, told the space moved.

### Placing the files

A workspace's configuration may define its agents. So a workspace's session
files go in place only once the restore that opens it reads that
configuration. The daemon keeps them with the removal record until then. It
deletes them once the client has placed them. Files the client could not place
stay, listed for the workspace's next removal record.

### Session IDs

The export records the agents' sessions with the exporting machine's ID when
it has one. An import keeps the session IDs on that machine, where the
package carried the session's files, or when told to keep sessions. The agents
resume those sessions from any directory. The import drops another machine's
terminal places and waiting wrappers.

Resuming skips the launch's trust setup. So every reopen trusts the
workspace's path for the agents that resume there.

Otherwise the moved recovery records drop their session, place and wrapper.
`shoal resume` then starts each agent through the fresh-agent launch, with its
trust setup and guidance, told the space moved.

## Who moves spaces

A move of another joined machine's space goes through the hub to that machine.
That machine runs it as it runs its own.

A move to a joined machine stays within the hub, so it takes no permission
key. A space's agent moves every space its scope reaches there, from any
machine, as it renames or converts them. A copy of its own space is the
exception: the copy opens beside the space, where a scoped caller creates
nothing. So that copy takes the `admin` permission.

A scoped caller's move of another machine's space to a file has its scope
checked on that machine, as for a space of its own.
[Who moves spaces](../reference/move.md#who-moves-spaces) gives the
permissions each destination takes.

`ssh` reaches outside the hub's trust. So only the user moves another
machine's space over SSH.
