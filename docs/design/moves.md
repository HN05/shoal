# Moves

This file covers moving spaces to another machine, or through a file, as
packages.

Moving a space to another machine ends it there and continues it here, through
one path for both kinds: the package holds the space's removal record, whose
kind it names, with the agents' sessions and their files, and the import records
it as removed here and restores it as `shoal restore` does, under its ID and
below its parent where that is here, as it does each space the package carries;
only recording it differs by kind. A package carries every space below its
space, whatever its kind, since removing the space takes them: one walk down
`space_parents`, with the chats in each carried worktree, gives them parents
first, so each opens below its parent again and a copied tree keeps its shape,
each copy below the copy of its parent. `--no-recursive` instead leaves the
spaces below here, moved up one level through the tree's parent link, and keeps
the branch a staying workspace builds on, so they keep running as they were. An
import resumes the agents of every space it opens, a chat's as a workspace's,
unless `--no-resume` leaves them stopped, over SSH as from a file, so a move
behaves the same whatever carries it. Import is a form of `shoal restore`,
`--from` a package, since it opens removed spaces as a restore does. `shoal
move` is one command whose destination is a flag, `--ssh`, `--machine` or `-o`,
so the space selection, `--keep`, `--checkpoint` and `--recursive` mean the same
for every destination and a new destination is one more flag; each destination
sets whether the spaces below go by default, and both flags are accepted with
every one. `--machine` leaves them by default, below the moved space, which
keeps its ID on the other machine, so the hub routes their events there and they
need no new parent; they are detached while its removal here runs, which would
take them along, and recorded with it until they are linked again, which a
restart during the removal finishes. Without a destination a terminal asks for
one, a joined machine first while another one is online, since the hub needs
nothing set up between the machines, and anything else is refused, so an archive
reaches standard output only through `-o -`. A move always names its machine: a
placement policy, which chooses where a new space starts, chooses no move's
destination. Each package uses the lowest format version that reads it, so an
older importer refuses as newer what it would misread; a chat package from
before chats moved as their removal records, and a workspace package from before
the record held the workspace's branch or starting commit, are refused. A move
runs in the daemon as an operation the command follows, as creation, `pr open`
and merge do: its worker checks the spaces as a batch does, and for each one has
the daemon stop the agents, saving their sessions, and write the package,
delivers it, and has the daemon end the spaces here once their destination has
it, so a move finishes even after the terminal that started it closes and is the
one move path every destination takes. Only delivery differs by destination:
`-o` writes the package where the daemon writes it, `-o -` hands it to the
command, which holds the move's lock and ends the spaces once it copied the
package, and `--ssh` pipes it into `ssh -o BatchMode=yes <host> shoal --json
restore --from -`, so the other side stays a plain command and the key must work
without a prompt, as Git's fetches need. `--machine` streams it to the other
machine's daemon over a duplex stream of the hub's links, which pipes it into
`shoal internal import-moved`, the same restore path, so that side too is a
plain command. That machine's claim of the space at the hub commits such a move:
it records the space as removed, then claims it, so a refused claim forgets the
record, and once the claim stands the record stays there for `shoal restore`
whatever fails after it. Each recorded move holds a ticket its destination's
claims carry; a source that cannot learn from the destination whether it claimed
the space asks the hub to settle the move, which answers from its claims and
owners and, when the destination has not claimed it, refuses that ticket's later
claims, so an import still running there cannot take the space after its source
restored it. While the hub cannot be asked, the source keeps the agents stopped
and asks again instead of guessing, so the agents never run on both machines. A
removed space's record moves to a joined machine through the same delivery and
claim, in one daemon request with no recorded move, since no agent runs to stop
and nothing ends here: the destination's daemon records it as removed without
opening it, and the source forgets the record once the claim stands, or once the
hub says it does, so a source that stops before it learns keeps the record, as
an export to a file does. The worker checks the spaces before any ends, and
where ending them needs the user it ends nothing and asks, so the command, or an
app, starts the move again with consent to what the question listed. A move
request for another joined machine's space goes through the hub to that machine,
which runs it as it runs its own; only the user moves a space over SSH from
another machine, since `ssh` reaches outside the hub's trust. `-o` writes where
the command runs, so the machine that has the space, or keeps its removal
record, only packs it and holds the package for the command's machine to read
through the hub; a scoped caller makes such a move as its `admin` permission
allows, its scope checked on that machine as for a space of its own. A move to a
joined machine stays within the hub, so it takes no permission key: a space's
agent moves every space its scope reaches there, from any machine, as it renames
or converts them, except a copy of its own space, which opens beside it, where a
scoped caller creates nothing, and so takes the `admin` permission. A move that
would stop the agent asking for it waits for its turn to end, as a conversion
does, recorded so a restart keeps it, since stopping the agent mid-turn would
cut off the command that asked. The daemon records each space's move, with its
destination and the stopped work its spaces saved, before any agent stops, and
the package's checkpoints and phase once written, and the worker holds a lock on
the space while it moves it. A recorded move whose lock is free lost its worker,
so the daemon finishes it from the phase its record reached instead of guessing,
in a worker of its own: a package that left ends the spaces here, nothing that
left moves again, and an import that may have happened leaves the agents stopped
on both machines, as a dropped connection does, except on a joined machine,
where it settles at the hub. A worker that outlives a daemon restart finishes
its move itself, losing only the reports its followers would have read. Moving a
workspace is a removal there and a restore here: export writes the removal
record and a Git bundle of its checkpoint from the daemon, and the destination
is checked before anything ends. Import records both as a removal of this
repository, the ref before the records, then runs the normal restore. Moving
rather than copying keeps one workspace per branch, so two agents never push it
or watch its PRs, and the source's removal stays restorable when the transfer
fails. A space already removed, a chat as a workspace, exports from its removal
record, which stays, so it remains restorable here. A copy, which leaves the
space running, keeps that rule by becoming new work under an ID of its own: a
workspace's copy is one of its checkpoints, which the import opens as a new
workspace on a branch of its own with its issue links but not its PRs or CI
runs, and every copied agent, a chat's included, starts from a fork of its
session under a new ID, so the original conversation never changes, even on the
same machine. A copy of an earlier checkpoint carries sessions newer than its
files, which export, import and the forked agents are told. The bundle leaves
out the default remote's default branch, which the importer fetches when it
lacks the commit the bundle builds on, since that branch only advances.
Repositories match by origin, and an import registers an origin no registered
repository has as a clone, as `start --machine` does, so a package needs nothing
set up before it arrives. The daemon reads a package itself, from a file or from
the bytes a client streams on its connection, and checks it whole before it
records anything, so every import takes one path whether a terminal, another
daemon or the hub delivers it; it records the spaces the package carries with
its own and opens its own, and the caller opens the rest in order, setting each
workspace up before the chats in its worktree. Exporting every workspace of a
repository checks each one before removing any, so one that cannot leave refuses
the set while all stay; a failure once removals begin leaves the exported ones
moved and the rest in place, each reported. Copying several spaces needs no such
check, since a copy removes nothing: one that cannot be copied is reported and
the rest are copied. Agent conversations are the agent tools' private files,
which Shoal never reads or changes. Shoal tells clients where they are without
reading them: the daemon reports the sessions a space's agents reported, and
`shoal_client` finds each one's files by ID where its tool keeps them, under the
client's environment or one the repository's agents ran with, as an export does.
An export copies each reported session's files unread, and an import puts them
at the same path under the importing agent's session directory, replacing a file
there only with one that continues it, as a session that went away and came back
does, and leaving that to the installed Shoal in a development build; a session
whose files cannot be read is left out with a warning and starts over. A
workspace's files go in place once the restore that opens it reads the
workspace's configuration, which may define the agent, so the daemon keeps them
with the removal record until then and deletes them once the client has placed
them; ones it could not place stay, listed for the workspace's next removal
record. The export records the agents' sessions, with the exporting machine's ID
when it has one, beside the moved records, which older importers read. An import
on that machine, one that received the session's files, or one told to keep
sessions, keeps the session IDs, which the agents resume from any directory,
dropping another machine's terminal places and waiting wrappers, and every
reopen trusts the workspace's path for those agents, since resuming skips the
launch's trust setup. Otherwise moved recovery records drop their session, place
and wrapper, and `shoal resume` starts them through the fresh-agent launch, with
its trust setup and guidance, told the space moved. Moving a chat removes it
here, stopping its agent, since a session that kept growing here would fork from
the one that continues there. Export asks first, whatever the space's kind,
exactly when it ends spaces besides the one named or releases holds. A space
whose agent reported no session moves with a warning, whatever its kind, its
agent starting as a new one there, told the space moved. Export, copy, move and
restore each run one path for a space of either kind, and only a workspace's
worktree step, its checkpoint and branch, differs. The spaces below a moving
space and the chats in each moving worktree move with it in its package as their
removal records, which the import records and restores, so they keep their IDs,
their parents and their worktrees wherever those are there. Import starts the
chat through the chat launch, in its mode, in the worktree of the workspace it
ran in where that workspace is there and ready, since workspaces keep their IDs
across moves, else in the repository's checkout or, for a chat without one, the
importing machine's `root_dir`, with the agent's resume arguments, under a free
name since its own may be taken there, in the background when no terminal
attends the import, as queued agents start. Export and import move work through
any byte stream, where the user runs both halves and chooses their order. `move
--ssh` runs both over SSH after checking Shoal answers there, and ends a space
here only once its import there succeeded, since a chat keeps no record once it
ends: it stops the agents of the space and of the spaces that go with it, saving
their sessions, exports it while they stay, and queues their restores here,
keeping no file, once the import there failed before recording anything; an
import that may have recorded the space there leaves them stopped, so its agents
never run on both machines. A space that cannot end here after its import, such
as a workspace that changed since its export, checked again once its removal
hooks ran, stays, existing on both machines, rather than losing the newer work.
Shoal keeps no registry of machines, and deciding when and where to move work
belongs to a caller outside the core.
