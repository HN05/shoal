# Moving spaces

This file says what `shoal move` does.

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

`shoal move [space]...` takes one destination: `--ssh <host>`,
`--machine <machine>` or `-o <file>`. Without one, on a terminal, it picks the spaces when none are
named, then asks for the destination's kind, `Joined machine` first where
another joined machine is online, `SSH host` or `File`, without `File` given
`--no-resume`, and picks the machine among the other joined machines online,
by name, listing the offline ones above them with when the hub last saw them,
or asks for the host or path; given `--shoal` it asks only for the host. It
refuses the spaces, repository or `--checkpoint` it cannot take first, and
without a terminal it is refused, naming each flag.

The daemon runs the move, so it finishes even after the terminal that
started it closes. It checks the spaces before it ends any, and where that ends spaces
besides those named or releases holds, it ends nothing and the command asks,
then starts the move again for what the user agreed to. The command follows
the move, printing a line for each space as it goes; with `--json`
it prints what the move did with each space, `moved` and `failed`. An
interrupt cancels the move before its next space, and a second one stops
following, leaving it running. Shoal records each space's move before its
agents stop, and finishes a move whose worker went, as when the machine
restarted during it, from how far it got: a space whose destination has its
package ends here, one nothing of which left moves again, one whose package
`-o -` had yet to write stays here with its agents' restores queued, and one
another machine, or what read a package `-o -` wrote, may have taken stays
here with its agents stopped, as when the connection drops. An alert tells how each one finished.
Until Shoal finished such a move, another move of that space is refused with
the error code `move_in_progress`.

`shoal move [space] -o <file>` writes a workspace or chat to a new file, or
to standard output for `-`, refusing a terminal, and ends it here once the file
holds it. Its argument names a workspace or chat, else the current workspace,
else a picker offers both. The daemon stops the space's agents, saving their
sessions, writes the file and ends the space, so only `-o -` needs the
command, which copies the package to standard output and then ends the
space. A space another joined machine runs goes the same way: that machine
packs it and keeps its package until this machine's daemon has read it
through the hub and written the file, or the command standard output, and
then ends it there, or for `--keep` deletes the copy's package there, which
it also deletes when no machine reads it within 30 minutes. It
refuses before ending anything when the file exists, when the daemon cannot
create a file in its directory within 30 seconds, or when `-` would stream from
a place that closes with the space. A workspace is removed as `shoal rm`
removes it, and its file holds its removal record and checkpoint; a chat's
file holds its removal record with its agent's session. A workspace whose HEAD
or unsaved files changed after its file was written stays here too, and the
report says so and names the `shoal rm` that removes it here. Every space below it, workspaces and chats alike, and
the chats in the worktree of each workspace among them, go with it in its file,
parents first, ended by the same removal, each workspace with a checkpoint of
its own. Before ending anything, the export refuses, listing each space that
fails, when one of them is a workspace whose uncommitted files a checkpoint
cannot keep, that another lifecycle operation holds or that holds the file, or
for `-o -` one the terminal closes with or the chat the command runs in. It asks,
unless given `--yes`, when it ends spaces besides the one named, listing each
with its kind and agent, or releases holds. Each checkpoint
travels as a Git bundle without the commits of the default remote's default
branch; a checkpoint that branch already has travels without one. Ignored files,
leases, permits, approvals, grants and holds stay behind; setup rebuilds what the
workspace needs. A space whose agent reported no session exports with a warning,
and its agent starts as a new one where it is imported.
A workspace or chat
already removed, as by cleanup, is exported from its removal record, which
stays, with the records of the spaces its removal took, under the refusals that
still apply, without another removal; the record of a space another joined
machine removed goes the same way, that machine packing it and deleting its
package once read, and stays there. The file's manifest names each record's `kind`, `workspace` or `chat`.

`--no-recursive`, with any destination, leaves the spaces below here, where
`--recursive`, the default except with `--machine`, takes them, except those named too, which go
in the file where it carries the space above them; the chats in the worktree of
a workspace it carries still go with it. Each space that stays keeps running,
with the spaces below it, and except on a move to a joined machine, which
leaves it below the moved space, moves up one level, below the exported
space's parent, or to the top level when that cannot take it; a workspace keeps
its branch and the branch it builds on, which the removal keeps, and loses only
its link to the exported workspace. The requests the exported spaces decided
for those that moved up pass to the nearest space above them with an agent, or
to the user. A removed
space exports with its own record only; the records of the spaces its removal
took stay here, and restoring one opens it at the top level.

`shoal move <space>... -o <directory>`, for several spaces or those the
picker selects when `-o` names an existing directory, and `shoal move
--repo <repository> --all -o <directory>`, for every workspace and running chat
of the repository, export each to `<id>.shoal` in the directory, named after
the space's ID since names repeat, creating it when missing. A space below
another of them goes in that space's file, so the directory gets a file for
each space no other selected space carries. It checks every space as a single export does, and
that no workspace is in the middle of another lifecycle operation, before
ending any: when one cannot be exported, it lists
each refusal and ends nothing. It asks once, as a single export does, before
ending them, then exports each space and lists the files it wrote; a
failure there leaves the remaining ones to export, and the command exits 1
naming each failed space. The spaces other joined machines run, or whose
removal records they keep, go there as a single file's space does, each machine's after this machine's; with
spaces of more than one machine, each other machine first checks its own
without ending any, and the one question lists what each would end.

`shoal move [space] --keep -o <file>` leaves the space and the spaces
below it running and writes copies of them instead: a workspace at a new
checkpoint, as `shoal checkpoint` takes one, or for the named workspace with
`--checkpoint N` at an earlier one `shoal restore <workspace> --checkpoint`
lists, and a chat with its session. It refuses a removed space.
With several spaces, a directory or
`--all`, it writes a copy of each space no other selected space carries to
`<id>.shoal` in the directory, reporting each space it cannot copy and copying
the rest. It refuses `--checkpoint` for
more than one space, since its number names a checkpoint of one workspace. The import
opens a workspace copy as a new workspace on the nearest free `-2`, `-3`, ...
variant of its branch, linked to its issues but not its PRs or CI runs, where
`shoal resume` starts its agents from forks of their sessions, as `shoal start
--fork` does, or as new agents told the workspace is a copy; a chat copy opens
as a new chat whose agent `shoal resume` starts from a fork of its session,
or as a new agent told the chat is a copy.
A copy of either kind opens under an ID of its own, below the copy of the
space it was below, and a workspace copy that builds on its parent's branch
builds on the copy's branch. Forks get session IDs of their own, so
the original conversations never change. A copy of an earlier checkpoint warns at
export and import that the agents' conversations describe work its files lack,
and tells the agents so. `restore --from` refuses `--keep-sessions` for a copy.

`shoal move [space]... --machine <machine>` moves spaces to another machine
joined to the same hub, named or by the ID `shoal hub ls` shows, from any
joined machine, where the machine that runs the spaces runs the move. Both
machines must be online: a move to this machine or an offline one is refused
before anything stops, naming when the hub last saw it. Only the named spaces
and the chats in the worktree of each moving workspace move; `--recursive`
takes the spaces below too, and the spaces that stay keep running below the
moved space, which keeps its ID and number there. The package goes over the
hub's links to that machine's daemon, which continues it as `shoal restore
--from -` does, cloning a repository it lacks, and resumes its agents unless
`--no-resume`, telling each where its space is now. Its claim of the space at the hub commits the move: a machine
that took none of the package leaves the spaces here, with their agents'
restores queued, and once its claim stands the spaces are its own and end
here, also when setting them up there fails, which the move reports and
`shoal restore <id>` retries there. When that machine cannot say how its
import went, as when the link drops, the move asks the hub whether it claimed
the space; while the hub cannot be asked, the agents stay stopped here and the
move asks again every 30 seconds, and once the hub answers, a machine that did
not claim the space no longer can. `--keep` copies the spaces there as new
spaces, without a claim. A removed space's record moves there too: that
machine records it as removed under its ID, claiming it at the hub, so
`shoal restore <id>` opens it there, and this one forgets it once that claim
stands, keeping it when the claim is refused and refusing its restore here
until the hub settles an arrival that machine could not report; an agent
moves the record of a space it may restore. A space's agent moves every space its scope reaches there, chats included,
with no permission key, also from another joined machine, and ends one the
user holds as its `user_holds` permission allows; a copy of its own space,
which would open beside it, takes its `admin` permission. A move that would stop its
own agent, as of its own space or the workspace its chat runs in, starts once
that agent's turn ends, and the agent continues on the other machine from its
session; one that leaves the space here tells the agent why.

`shoal restore --from <file>` hands an export, or standard input for `-`,
refusing a terminal, to the daemon, which reads it whole, refusing an export
another Shoal version wrote, and continues it in the registered repository with the
exported repository's origin, or the one `--repo` names, or a chat without a
repository in `root_dir`. A space the file carries goes to the repository
with its own origin, or where the file goes when it is of the file's
repository. Where no registered repository has an origin the import needs,
it registers that origin as a clone, as `shoal start <url>` does; a
workspace process's import clones none. Given `--from` several times, or a
directory, whose `.shoal` files it reads in name order, it imports each one, going
on after a failure, lists every space it continued and exits 1 naming each file it
could not import or whose `hooks.post_setup` failed. The export carries the files of each
session the agents reported, such as Claude Code's and Codex's transcripts, and
the import puts them where the agent looks for them, keeping a file already
there unless the imported one continues it, so the session continues by ID on any machine; a session
whose files the export could not read is left out with a warning, and continues
only on the machine that exported it. `--keep-sessions` keeps every session,
such as one whose files were copied by hand, and `--new-sessions` none.

An imported space is recorded as removed, a workspace with its checkpoint once
the repository fetched the default branch when it lacks the commit the bundle
builds on, then restored as `shoal restore` does, under its ID; when that
restore fails, `shoal restore <id>` retries it. The agents' session files stay
with the removal record, in the repository directory's `.sessions`, or in
`root_dir`'s for a chat without a repository, until the restore that opens the
space puts them in place, as its own
configuration describes the agents, and an export of that record carries them
again. Files that restore cannot put in place, as in a development build
without `SHOAL_DEV_USER_FILES`, stay for the space's next removal record.
The import queues the restores of the agents the export stopped, as `shoal
restore` does, under the importing machine's agent limits. Every reopen, by import or
`shoal resume`, trusts the workspace's path for the agents that resume there. An agent
without a kept session `shoal resume` starts as `shoal start --in` starts a new
one, telling it the workspace moved, instead of running
its `resume_command`. An imported chat opens as `shoal restore` reopens a
removed chat, stopped, in the worktree it ran in where the importing machine
has that workspace ready, such as one that moved there before, otherwise in
the repository's checkout with a warning, or in `root_dir` without a
repository. The spaces a file carries open again after its space the same way,
parents first, each below its parent and each workspace set up before the
spaces below it; one that cannot open fails the import, which goes on with the
rest and names each failure, so `shoal move --ssh` keeps the spaces here, and its
record, where the import made one, stays for `shoal restore <id>`. With `--no-resume`, the agents of every imported space
stay stopped and the report names the `shoal resume` that resumes them.
An import is refused, before it records anything, when a space with the ID of
the package's space, or of a space it carries, is already here: `shoal rm`
removes that space here, or `shoal move --keep` where the package was written
writes a copy, which opens under an ID of its own. An import that fails before
it records anything of the space, such as one whose clone fails or whose
package ends early, reports the error code `not_imported`. Workspace
processes can neither export nor import.

`shoal move [space]... --ssh <host>`, or `--repo <repository> --all`, moves each
space with `ssh -o BatchMode=yes <host> shoal --json restore --from -`, after
`ssh <host> shoal repo ls` shows Shoal answers there; when it does not, nothing
ends. The daemon runs `ssh`, which never prompts there: the host must take a
key that works without a prompt, as Git's fetches need, and a host alias is
read from the SSH config of the machine that runs the move. On a machine
joined to a hub, the move of another machine's space runs on that machine, as
the user's, so its `ssh` reaches the host. `--shoal <path>` names the program there when the remote shell's `PATH`
lacks it, and `--no-resume` leaves the agents stopped there, a chat's as a
workspace's. It checks the spaces as a batch export does and
asks once before ending them. Each space then moves on its own, after the
spaces below it: Shoal stops its agents and those of the chats that go with it,
saving their sessions, exports it while they stay here, and imports it there, and only once that import succeeds
ends it here through the export's removal. When the export fails, `ssh` cannot
run, or the import fails with `not_imported`, the space stays here, its stopped agents' restores are queued, and the report gives the other
machine's error. When the import fails otherwise, as when setup there fails or
the connection fails or drops, the space stays here with its agents stopped, since the
other machine may have it, and the report names the `shoal rm` that removes
it here once it arrived there and the `shoal resume` that continues it here
otherwise. A workspace with a space below it
that stayed is not exported and stays here. When the import succeeds but the
space cannot end here, as for a workspace whose HEAD or unsaved files changed
since its export, its own `hooks.pre_remove` changing them included, or whose
`hooks.pre_remove` fails, it stays here too, and the
report says it now exists on both machines and names the `shoal rm` that
removes it here.
`--keep`, with `--checkpoint N` for one workspace, copies the spaces instead, as
`shoal move --keep -o` does, and imports each copy there as a new space,
leaving the space running here; it asks nothing, and a space it cannot copy or
import there is reported while the rest are copied.
