# Links and watches

This file covers `shoal-links`: the issues, PRs and CI runs spaces link, the
polls that read them and the watches that report them.

External branch changes are observed during daemon sweeps without changing
recorded ownership. Issue and PR association changes append `linked` or
`unlinked` events in the same transaction as the association mutation, carrying
the item's kind, canonical URL and relation so integrations can follow links
without polling workspace inspection. Item selectors make the `pr`/`issue` word
optional for numbers: a linked item with that number decides the kind, otherwise
the forge's issue read where issues and PRs share one sequence. Where they do
not, an unlinked number stays ambiguous and needs its kind rather than a guess
between two reads. Ready workspaces and the chats of a repository with linked
issues or PRs are polled every 30 seconds, whether or not their agents run,
through one path, so an asleep space hears what wakes it; the daemon keeps a
separate activity snapshot for each item and appends an `item_activity` event
for each newly observed comment, edit, completed check, conflict, closure,
merge, reopen, or lookup failure. The first observation establishes the
baseline, and watch acknowledgements do not affect this stream. The same poll
queues each change on the space's watch cursor, holding passing checks as the
watch below describes, and wakes a running watch, so an agent receives activity
without watching for it. A poll reads spaces, their items, an item's independent
parts and the agent queue's issue dependencies with bounded concurrency, so one
slow forge does not hold back the rest. Polls read through the forge crate's
shared request layer, which remembers each account's last response per URL in
daemon memory and repeats the read as a conditional request, so an unchanged
item costs a `304 Not Modified`, which GitHub does not count against its primary
rate limit. The memory is not persisted: a restart reads everything once in
full. The same layer reads each forge's rate-limit headers and sends nothing to
a forge that limits the account until the limit ends; polls wait for it and the
user gets one alert, because a lookup failure would ask an agent to act on what
only time fixes. A 429 is such a limit, never resent within seconds as a 503 is.
The layer sends a read again after a failure that may pass, since reading twice
changes nothing and one refused connection would otherwise fail a whole poll,
and a source's lookup failure reaches the agent at most once per report
interval, since an intermittent failure varies its error and passes between
polls. A failure that repeats reaches it once, until its source is read or fails
differently: each repeat would start an idle agent's turn with nothing new. A
failed read of a linked item itself is such a source: it keeps the item's last
snapshot and reports `lookup_failed`, so an outage never also fails the watch or
the Stop hook's wait that reports it. A watch of an item named on the command
line still fails, so a mistyped item does not wait silently. Decision: with
`[pr] watch_merge_runs` on, a repository has Shoal follow the default branch's
CI runs for the merge of each PR a space links as assigned or opened, since an
agent that slept or finished never heard that its merge broke the default
branch, and two agents then found and filed the break twice. The background poll
reads an opened PR, which no watch follows, only until it sees it merge or
close, and neither reports nor logs its other activity, since an opened link
does not ask for it. The first read that records the merge, a watch's or the
background poll's own, whose record no delivery the agent has not taken holds
back, starts a persisted lookup of the merge commit the forge reports, never one
Shoal guesses; the background poll lists its runs for a fixed ten minutes, since
a run that has not started by then is not coming, and links each as related, so
the linked-run watch reports its end through the paths every update takes. The
lookup and its unfinished runs keep the space from unattended removal, so a done
space stays to hear them, however long a run waits for a runner. A `shoal merge`
that pushes the default branch starts the same lookup for the tip it published,
once the tip holds the merged branch and the upstream, as Git tracks it after
the push, holds the tip, so a retry that publishes a merge an earlier push
failed to is followed too. It lists the runs on the forge of the first URL Git
pushed to on a network host, so a push only to local repositories starts none; a
poll that cannot find that host's forge tries again until the window ends, then
ends the lookup, so a host that serves no forge never keeps the space. A
repository that merges locally thus hears of a broken default branch through the
same path; lookups are keyed by their commit, which a PR's or a push's names.

Issue linking and unlinking are own-workspace operations; associations are
idempotent and replacing one requires unlinking it first. Unlinking preserves
recorded completion. Issue associations suppress idle cleanup, not merged
cleanup. Item views let agents read issues and PRs without knowing the forge.
The daemon selects linked or explicit items, gives a bare number the kind of the
linked item or else the forge's, and reads them through the forge's API with the
account a read uses, or anonymously without a token, sending a view whose
discussion exceeds a protocol frame in pieces. Views automatically include the
logs of failed CI jobs where the forge serves them, fetched by the daemon with
the workspace's agent account and returned as bounded tails with the job's URL,
so scoped callers never need forge credentials. Successful and pending checks
omit logs, and a check that links no CI job in the repository has none to read;
explicit selection limits output to one failed check. Failed-check watch updates
include its failing step when recorded by the runner; an unavailable log is
reported on that update without hiding other activity. Agents can also rerun
failed and cancel running CI jobs on a PR's current head through the daemon, as
the agent account and under the same scope, with each action recorded as an
`item_changed` event. Each forge's own job API carries them out; Forgejo's API
reruns jobs only from release 17, which Shoal reads from the server's version,
and Shoal does not sign in to its web UI, so earlier Forgejo releases and Gitea
only cancel, and Shoal acts on no Bitbucket Pipelines job. Agents may also start
a workflow run on the branches whose runs they act on, on every forge including
Bitbucket. A started run is not a rerun of a PR's checks: on Forgejo it has no
head commit or event, so it cannot replace the rerun Forgejo lacks before 17,
while elsewhere only a run on the PR's head commit reports checks there. `ci`
gathers both: it reruns and cancels one run by its URL, or the checks on a PR's
head without one, and `view ci` reads a run as `view` reads an issue or PR. Run
numbers overlap issue and PR numbers, so only a number after `ci` names a run.
Scoped processes act only on runs of the default branch and of the branches of
the workspaces their scope reaches, in the repository they act in, which the
daemon reads from the run before acting, and read any run of their repository. A
run or workflow URL, or `--repo`, of another repository is read by anyone and
acted on by the user: the daemon starts, links, reruns and cancels runs there
with the user's own logins, as it merges, also outside a workspace, and refuses
workspace processes those actions, which stay within their own repository. A
workflow file's URL also names the branch a run starts on. Forges hold the runs
of fork and first-time contributors' PRs for approval, which leaves such a PR
without checks; `view` and `status` report the waiting runs beside the checks,
which stay exactly what the forge reports, so watches and their wake-ups do not
change. Approving lets outside code run, so like merging it uses the user's
login and is refused to workspace processes. Only GitHub has an approve API;
Forgejo approves through a web UI route, which Shoal does not use. Viewing a run
reads every page of its jobs; the daemon sends them, and listed runs, in
frame-sized pages, and each failed job's log, named by job ID, as one request,
like a PR's, so no response outgrows a protocol frame.

Persisted watches use the user's forge login, resolve numbers against the
workspace's origin into stored URLs bound to that repository. Top-level `link`
and `unlink` manage associations and always name a relation, so no link becomes
the assignment by default. `assigned` items are the work the workspace exists
for and drive completion and cleanup; `opened` and `related` items are records
for integrations without lifecycle effect, and Shoal records `opened` itself for
the issues, PRs and CI runs it creates. A CI run is a linked item of its own
kind, never `assigned`, because a run cannot complete an assignment: watches
follow it until they deliver its finish, since a dispatched Forgejo run belongs
to no PR and a run can outlive its PR's watch. A PR's runs are not linked for
it, as its checks already cover them. A chat links the runs it starts as a
workspace does, but its checkout's branch is not its own: `ci ls` defaults to
the runs on its linked PR's head, its other CI commands act on that PR's checks,
and `ci run` starts on the default branch unless `--branch` names another. A
space links each item under one relation, so every list, watch and cleanup rule
reads one answer for it: assigning an item replaces its opened or related link
without dropping its replay cursor, an assigned item refuses another relation,
and opened and related replace each other. Unlinking selects a relation's items,
one kind of them, or one item. PR links accumulate without duplicates and
require every watched PR to merge, with HEAD present in at least one or merged
into the freshly fetched default branch, as a rebase after the merge leaves it.
A PR's head need not be the workspace branch, since an agent may push stacked
PRs from its worktree on other branches. PRs are looked up only once the
assignment is done, and confirmation proceeds to cleanup. The confirmed HEAD
persists so restart cannot query the same watch set again. Persisted manual
acknowledgement binds to exactly the recorded HEAD. Registrations distinguish
watches from acknowledgements. Ambiguous records and conflicting actions are
rejected. Own-workspace `watch` polls linked issue and PR activity independently
of cleanup, with kind filters or an explicit item that does not change
associations. It reports comments or reviews, edits, completed CI checks, new
merge conflicts, closure, reopening or merging. An edit is a change to what the
item's own read reports besides its state: its title, description, labels,
assignees, milestone and, for a PR, its draft state and base branch, named in
one update per poll. Comparing what that read already returns costs no request
per kind of event, as following the forge's timeline would, and works the same
on every forge; changes no read reports, such as requested reviewers, are not
followed. A snapshot saved before details were recorded takes the next read as
its baseline. Edits a workspace makes through Shoal, and Shoal's retargeting of
a stacked PR, are taken as seen on its cursor as its own comments and state
changes are, and on the cursors of the spaces below it whose activity its agent
receives, so a coordinator does not hear its change to a child's PR back while a
child's own agent does: each field the edit changed, and of labels and assignees
only the names it added or removed, so another edit read in the same poll is
still reported. Only edits count, since what the reads around any other action
show changed is someone else's. A space that comes below the acting space during
or after a change has no record of it: its cursor holds the change as someone
else's once its own poll reads it, so its new coordinator can hear the change
back once. Shoal accepts that echo. Holding the gates of the spaces above for
every move into a subtree would make each move wait for their forge requests and
still miss what the joining space polled earlier, and a cursor records what
changed, not who changed it. An assignee Shoal adds or removes on its own, as an
agent starts or stops or a link comes or goes, is taken as seen on every cursor
of the issue, since it is nobody's activity. It is changed under the issue's
assignee gate, not the PR gates, which callers that link, unlink and remove
spaces already hold for one space and could not take for the others without
deadlock; a poll that records an issue while that gate is held reports none of
its details or state as changed, so what it read before Shoal recorded the
change stays unreported, and another edit it read arrives with the next poll.
Passing checks wait on the cursor while another check on the PR's head commit is
pending, since a pass asks nothing of the agent until the rest finish and each
wake costs it a turn: a check that does not pass delivers them at once, the last
pending one with itself, and a new head drops them. A check that never finishes
keeps them held; `status` shows them meanwhile. Holding happens where the poll
queues activity, so watches and agent hooks report the same updates, and only
for a PR's checks; the `item_activity` stream reports each one. A conflict
belongs to the head and base commits it was seen at, so a push or base change
that leaves the item conflicting is reported again. A forge's conflict is
reported once two polls in a row see it at the same commits, because Forgejo
shows the previous result until it rechecks and a PR it is still checking as not
mergeable. An unfiltered watch without a linked PR runs the `conflicts` check on
each poll as a cursor item named by the branch, so work without a PR still
learns of conflicts. It leaves out conflicts with a target that already has
HEAD's changes, as merged cleanup decides, since merging could only repeat them;
`conflicts` still reports them. Once the assignment is done, neither the watch
nor the background poll checks the branch: the work is finished, and a failed
check would only wake its agent. A linked PR leaves ordinary conflicts to the
forge, avoiding duplicate reports, and the watch and the background poll check
the branch beside it only for files changed on both sides, which no forge
reports, and only where the repository lists sensitive files. Each check against
the default target is kept per workspace with its head and target commits.
Forgejo reads come from its REST API and work anonymously for public
repositories. The watch shares a persistent cursor per workspace and item and
reports changes between its reads. Linking an item starts its cursor from a read
made then, and an agent that begins a new session, rather than continuing or
forking one, registers as such, so before its command runs the daemon starts
each followed item's cursor again from a new read: what the item's discussion
and details held by then is history, which `shoal view` shows, while its
condition, such as failed checks, a conflict or its closing, is still reported.
A poll that began before the read, by the order in which the daemon numbers its
reads, is dropped whenever it finishes, since it may show the item before the
read, and the agent's first message counts the issue's comments. A watch of an
item the space does not follow, or whose first read failed, reports existing
activity on its first read. Cancelling the watch discards that cursor. An
omitted timeout keeps the watch unbounded; an explicit timeout remains a bounded
polling interval. Activity lookups run outside completion serialization;
persisting results rechecks the watch set so cancelled watches cannot recreate
their cursors. Unacknowledged deliveries persist with the cursor so timeout or
disconnection cannot discard unseen updates; the CLI acknowledges after
successful output. They are stored apart from what the cursor saw of each item,
so hook deliveries and wake checks read only queued updates, never an item's
discussion history. Waits share the cursor, so a newer wait in a workspace
supersedes the running one before it takes updates, and an acknowledgement
arriving while a newer wait runs leaves the updates pending for it; competing
waits would consume updates the agent never reads. Unavailable activity sources
are explicit failures, never successful checks or proof of mergeability; a
persisting failure is reported again on an interval so it cannot silence a wait
while hiding a new conflict. An item whose activity has ended, a merged PR or,
once the assignment is done, a closed item, is not looked up again, so an
unreachable forge cannot fail or wake the wait of a finished workspace. Waiting
does not resume stopped agents or grant merge permission; agent instructions
direct agents to handle updates and wait again.
