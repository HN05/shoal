# Links and watches

This file covers `shoal-links`: the issues, PRs and CI runs spaces link, the
polls that read them and the watches that report them. The
[links reference](../reference/links.md) says what `link`, `unlink`, `view`
and `watch` do, and the [CI reference](../reference/ci.md) what the `ci`
commands do.

## Links

### Relations

A space links an item under a relation. `assigned` items are the work the space
exists for, and they drive completion and cleanup. `opened` and `related` items
are records for integrations, without lifecycle effect. Shoal records `opened`
itself for the issues, PRs and CI runs it creates.

`link` and `unlink` always name a relation, so no link becomes an assignment by
default.

A space links each item under one relation, so every list, watch and cleanup
rule reads one answer for it. Assigning an item replaces its opened or related
link and keeps its watch cursor. An assigned item refuses another relation
until it is unlinked. Linking an item again under its relation changes nothing.

Links neither complete a space nor withdraw its completion. Only `done` does,
as [What completes an assignment](cleanup.md#what-completes-an-assignment)
says.

### Link events

Linking and unlinking append a `linked` or `unlinked` event in the same
transaction as the change. The event carries the item's kind, canonical URL and
relation. Integrations thus follow links without polling the spaces.

### Item selection

A link stores the item's URL, resolved against the workspace's `origin`, so it
stays bound to that repository.

A bare number takes the kind of the linked item with that number. Otherwise it
takes the kind the forge's issue read reports, where issues and PRs share one
sequence. Where they do not, an unlinked number needs its kind, since Shoal
could only guess between two reads.

Run numbers overlap issue and PR numbers, so only a number after `ci` names a
run.

### Assigned PRs

Assigned PRs accumulate without duplicates. A PR's head need not be the
workspace's branch, since an agent may push stacked PRs from its worktree on
other branches.

A done workspace waits for its assigned PRs as
[Watched PRs](../reference/completion.md#watched-prs) says. Shoal looks the PRs
up only once the assignment is done, and a confirmation proceeds to cleanup.
The confirmed HEAD persists, so a restart does not query the same PRs again.

A manual acknowledgement binds to exactly the recorded HEAD. Registrations tell
watches from acknowledgements. Shoal rejects ambiguous records and conflicting
actions.

### CI runs

A CI run is a linked item of its own kind. It is never assigned, because a run
cannot complete an assignment.

Watches follow a run until they deliver its finish. A dispatched Forgejo run
belongs to no PR, and a run can outlive its PR's watch.

A PR's runs are not linked for it, since its checks already cover them.

## Chats' links

A chat holds links as a workspace does. The link tables name the space, a
workspace or a chat, by an ID from their shared namespace, and the links go with
the space.

A chat's assigned items neither complete it nor clean it up: only its own
`done` records its completion. A watched PR keeps a chat from idle cleanup, but
not a done chat from completion cleanup. A chat has no branch whose work an
unmerged PR would keep.

A chat's checkout branch is not its own. Its CI commands therefore default to
its linked PR's head, and `ci run` to the default branch, as
[Where the commands act](../reference/ci.md#where-the-commands-act) says.

The activity Shoal polls for a chat's items waits on the chat's own cursor. The
chat's hooks deliver it with the chat's messages, which wakes its agent. A
watch in the chat and those hooks share one delivery, so the newest takes what
is pending.

## Assignees on the forge

An assigned issue shows on the forge who works it, through its assignees. Every
forge has assignees, while a label would need one per repository.
[Assignees on the forge](../reference/links.md#assignees-on-the-forge) says
when each account is assigned.

Every Shoal that acts as the same account shares that assignee. Each Shoal
therefore records the issues it put or kept the account on, and takes it off
only those. It leaves the account on an issue another Shoal's agent may work.
It reads the forge for whether the account is there, and settles assignees in
the background, so a slow forge holds back no start or stop. Once the
background poll sees an issue lose an assignee, the spaces that link it settle
the account again. An issue another Shoal took the account off thus gets it
back while an agent here works it.

Shoal records, per issue, each of the user's logins it added. It takes back
only those, and only once no space links the issue for them, never a login the
user assigned themselves.

A space's links carry whether an agent assigned each issue, and the user's
login Shoal added. A restore or a move thus assigns as before and keeps Shoal's
ownership. A moved space leaves the agent account to the machine it moved to.

Assigning never refuses or undoes the link or the transition. Other assignees
and forge failures come back as warnings or alerts.

## Polling

### The background poll

The daemon polls the linked items of ready workspaces, and of the chats of a
repository, every 30 seconds. It polls them whether or not their agents run,
through one path, so an asleep space hears what wakes it.

The poll keeps an activity snapshot of each item. It appends an
`item_activity` event for each newly observed comment, edit, completed check,
conflict, closure, merge, reopening or lookup failure. The first observation
sets the baseline. Watch acknowledgements do not affect this stream.

The same poll queues each change on the space's watch cursor and wakes a
running watch. An agent thus receives activity without watching for it, as
[Alerts, threads and changes](coordination.md) says.

### Reads

Polls and watches read with the user's own logins. Their reads go through the
forge's request layer, where an unchanged item costs only a conditional request,
as [Repeated reads](forge.md#repeated-reads) says.

A poll reads spaces, their items, an item's independent parts and the agent
queue's issue dependencies with bounded concurrency. One slow forge thus holds
back none of the rest.

Activity lookups run outside completion serialization. Saving their results
checks the watch set again, so a cancelled watch cannot recreate its cursor.

A merged PR is not looked up again, nor a closed item once the assignment is
done. An unreachable forge thus cannot fail or wake the wait of a finished
workspace.

## What a watch reports

A watch reports comments and reviews, edits, completed CI checks, new merge
conflicts, closing, reopening and merging.
[What a watch reports](../reference/links.md#what-a-watch-reports) gives each
update.

### Edits

An edit is a change to what the item's own read reports besides its state: its
title, description, labels, assignees, milestone and, for a PR, its draft state
and base branch. One update per poll names them.

Comparing what that read already returns costs no request per kind of event, as
following the forge's timeline would. It also works the same on every forge.
Changes no read reports, such as requested reviewers, are not followed.

### Changes a space made itself

The edits a workspace makes through Shoal, and Shoal's retargeting of a stacked
PR, count as seen on its cursor, as its own comments and state changes do. They
also count as seen on the cursors of the spaces below it whose activity its
agent receives. A coordinator thus does not hear its change to a child's PR
back, while a child's own agent does.

Only what the edit changed counts as seen: each field it changed and, of labels
and assignees, the names it added or removed. Another edit read in the same poll
is thus still reported. Only edits count, since what the reads around any other
action show changed is someone else's.

A space that comes below the acting space during or after a change has no record
of it. Its cursor holds the change as someone else's once its own poll reads
it, so its new coordinator can hear the change back once. Shoal accepts that
echo. Holding the gates of the spaces above for every move into a subtree would
make each move wait for their forge requests. It would still miss what the
joining space polled earlier, since a cursor records what changed, not who
changed it.

An assignee Shoal adds or removes on its own, as an agent starts or stops or a
link comes or goes, counts as seen on every cursor of the issue. It is nobody's
activity.

Shoal changes such an assignee under the issue's assignee gate, not the PR
gates. Callers that link, unlink and remove spaces already hold the PR gate of
one space, and could not take the others' without deadlock. A poll that records
an issue while its assignee gate is held reports none of its details or state as
changed. What it read before Shoal recorded the change thus stays unreported,
and another edit it read arrives with the next poll.

### Held checks

Passing checks wait on the cursor while another check on the PR's head commit
is pending. A pass asks nothing of the agent until the rest finish, and each
wake costs it a turn.

A check that does not pass delivers the held passes at once, and the last
pending check delivers them with itself. A new head drops them. A check that
never finishes keeps them held, and `status` shows them meanwhile.

Checks are held where the poll queues activity, so watches and agent hooks
report the same updates. Only a PR's checks are held. The `item_activity`
stream reports each check.

### Merge conflicts

A conflict belongs to the head and base commits it was seen at. A push or base
change that leaves the item conflicting is thus reported again.

A forge's conflict is reported once two polls in a row see it at the same
commits. Forgejo shows the previous result until it rechecks, and shows a PR it
is still checking as not mergeable.

An unfiltered watch without a linked PR runs the
[`conflicts`](git.md#conflicts) check on each poll, as a cursor item named by
the branch. Work without a PR thus still learns of conflicts. The check leaves
out a target that already has HEAD's changes, as merged cleanup decides, since
merging could only repeat them. `conflicts` itself still reports them.

A linked PR leaves ordinary conflicts to the forge, which avoids duplicate
reports. The watch and the background poll then check the branch only for
[files changed on both sides](git.md#files-changed-on-both-sides), which no
forge reports, and only where the repository lists such files.

Once the assignment is done, neither the watch nor the background poll checks
the branch. The work is finished, and a failed check would only wake its agent.

Each check against the default target is kept per workspace with its head and
target commits.

### Failed lookups

An unavailable activity source is an explicit failure, never a passing check or
proof that a PR merges cleanly.

A failed read of a linked item keeps the item's last snapshot and reports
`lookup_failed`. An outage thus never fails the watch, or the Stop hook's wait
that reports it. A watch of an item named on the command line still fails, so a
mistyped item does not wait silently.

A source's lookup failure reaches the agent at most once per report interval.
An intermittent failure varies its error and passes between polls, so each
failure would otherwise end a wait. A failure that repeats reaches the agent
once, until its source is read or fails differently, since each repeat would
start an idle agent's turn with nothing new.

A forge's rate limit makes polls wait instead of reporting lookup failures, and
the user gets one alert. A lookup failure would ask an agent to act on what only
time fixes.

## Cursors

### A cursor per item

A watch keeps a persistent cursor per workspace and item, and reports what
changed between its reads.

Linking an item starts its cursor from a read made then. An agent that begins a
new session, rather than continuing or forking one, registers as such. Before
its command runs, the daemon starts each followed item's cursor again from a new
read. What the item's discussion and details held by then is history, which
`shoal view` shows, and the agent's first message counts the issue's comments.
The item's condition, such as failed checks, a conflict or its closing, is
still reported.

The daemon numbers its reads in order. A poll that began before a cursor's new
read is dropped whenever it finishes, since it may show the item as it was
before that read.

A watch of an item the space does not follow, or whose first read failed,
reports existing activity on its first read. Cancelling the watch discards that
cursor.

### Deliveries

Unacknowledged deliveries persist with the cursor, so a timeout or disconnection
cannot discard unseen updates. The CLI acknowledges them after successful
output.

Deliveries are stored apart from what the cursor saw of each item. Hook
deliveries and wake checks thus read only queued updates, never an item's
discussion history.

### Waits

Waits in a workspace share its cursor, so a newer wait supersedes the running
one before it takes updates. An acknowledgement that arrives while a newer wait
runs leaves the updates pending for it. Competing waits would consume updates
the agent never reads.

Waiting neither resumes stopped agents nor grants merge permission. Agent
instructions direct agents to handle updates and wait again.

## Merge runs

An agent that slept or finished never heard that its merge broke the default
branch, and two agents then found and filed the break twice. Decision: with
`[pr] watch_merge_runs` on, Shoal follows the default branch's CI runs for the
merge of each PR a space links as assigned or opened.
[Merge runs](../reference/links.md#merge-runs) says what the space sees.

The background poll reads an opened PR, which no watch follows, only until it
sees the PR merge or close. It neither reports nor logs the PR's other activity,
since an opened link does not ask for it.

The first read that records the merge starts a persisted lookup of the merge
commit the forge reports, never one Shoal guesses. That read may be a watch's or
the background poll's own. No delivery the agent has not taken holds its record
back.

The background poll lists the commit's runs for a fixed ten minutes, since a
run that has not started by then is not coming. It links each run as related,
so the linked run's watch reports its end through the paths every update takes.

The lookup and its unfinished runs keep the space from unattended removal. A
done space thus stays to hear them, however long a run waits for a runner.

Lookups are keyed by their commit, which a PR or a push names.

### Local merges

A repository that merges locally hears of a broken default branch through the
same path. A `shoal merge` that pushes the default branch starts the same
lookup for the tip it published. It starts once the tip holds the merged branch
and the upstream, as Git tracks it after the push, holds the tip. A retry that
publishes a merge an earlier push failed to publish is thus followed too.

The lookup lists the runs on the forge of the first URL Git pushed to on a
network host, so a push only to local repositories starts none. A poll that
cannot find that host's forge tries again until the window ends, then ends the
lookup. A host that serves no forge thus never keeps the space.

## Views

Item views let agents read issues and PRs without knowing the forge. The daemon
reads them through the forge's API with the account a read uses, or anonymously
without a token.

Views include the logs of failed CI jobs where the forge serves them. The
daemon fetches them with the workspace's agent account and returns bounded
tails with the job's URL, so scoped callers never need forge credentials.

### Frame-sized responses

No response outgrows a protocol frame. The daemon sends a view whose discussion
exceeds a frame in pieces. Viewing a run reads every page of its jobs, and the
daemon sends them, and listed runs, in frame-sized pages. Each failed job's log
is one request, named by job ID, as for a PR.

## CI actions

Agents rerun failed and cancel running CI jobs through the daemon, as the agent
account and under their scope. Each action is recorded as an `item_changed`
event.

Each forge's own job API carries the actions out. Shoal does not sign in to a
forge's web UI, so it uses no web UI route, even where the API lacks an action,
such as Forgejo's approval of a waiting run.
[Rerun or cancel](../reference/ci.md#rerun-or-cancel) says which forges do
what. Forgejo's API reruns jobs only from release 17, which Shoal reads from
the server's version.

### Started runs

Agents also start workflow runs on the branches whose runs they act on, on every
forge. A started run is not a rerun of a PR's checks. On Forgejo it has no head
commit or event, so it cannot replace the rerun Forgejo lacks before 17.
Elsewhere only a run on the PR's head commit reports checks there.

### Scope

A scoped process acts on the runs
[What an agent acts on](../reference/ci.md#what-an-agent-acts-on) lists. The
daemon reads a run's branch from the run before it acts.

A run of another repository follows the rule for
[other repositories](forge.md#other-repositories): anyone reads it, and only
the user acts on it, with the user's own logins.

### Runs that wait for approval

Forges hold the runs of fork and first-time contributors' PRs for approval,
which leaves such a PR without checks. `view` and `status` report the waiting
runs beside the checks. The checks stay exactly what the forge reports, so
watches and their wake-ups do not change.

Approving lets outside code run. Like merging, it is therefore the user's, with
their own login. A scoped process approves as the agent account only as its
`ci_approve` permission allows.

## The crate's boundary

The `Links` service keeps the watches' running waits. It finds where a
request comes from and the repository it acts in, and reads the forge. It also:

- links items to spaces under their relations, and keeps the spaces' PR
  watches, merge acknowledgements and merge runs,
- polls linked items, in watches and in the background,
- starts, reads, reruns, cancels and approves CI runs,
- lists and deletes the forge's branches, and reads failed check logs and item
  views,
- applies actions to linked items as the space's own changes, with the
  warnings, bindings and review gate of their merges,
- moves the workspaces stacked on a base once the base's work merges,
- opens issues and PRs and reverts merged ones, or proposes them where a
  permission asks, and runs the pushes an approval starts,
- delivers the activity to agents, or through the hub to coordinators on other
  machines.

The approvals subsystem calls the service to decide a proposal or finish its
push. The spaces, permission checks, the agents' records and messages, the
approval requests' records and routing, the reviews and the hub stay with the
daemon, which the crate's interface reaches. Whether an idle agent is due a
wake is the agent queue's.

The crate's record functions read and write what spaces link. Other
subsystems call them in their own transactions.
