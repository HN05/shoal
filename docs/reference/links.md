# Links and watches

This file says what `shoal link`, `unlink`, `view` and `watch` do, and how
linked items keep a space.

`shoal link <number-or-url> <relation>` associates an issue or PR with a space,
the workspace it runs in, else the chat, or the one `--space` names; `shoal link issue <number-or-url> <relation>` and `shoal link pr
<number-or-url> <relation>` select the kind explicitly, and omitting the item picks
an open one. `shoal link ci <number-or-url> <relation>`, or a run's URL alone,
links a CI run; a number alone never names one, since run numbers overlap issue
and PR numbers. The relation is required:

| Relation | Effect |
|---|---|
| `assigned` | The work the workspace exists for: the rules below and in [ready for review](completion.md#ready-for-review) apply |
| `opened` | Records that the workspace created the item; `issue open` and `pr open` record what they create and do not assign |
| `related` | Records an item the work relates to |

A space links each item under one relation. Assigning an item replaces its
opened or related link, linking an opened item as related or a related one as
opened replaces that link, and an assigned item is refused another relation
until it is unlinked.
Opened and related issues and PRs have no watch, completion or cleanup effect;
`status pr <number>` finds the workspaces that link an item under any relation.
A run is linked only as opened or related, since an assigned item drives
completion and cleanup, and records its workflow as its title. `shoal ci run`
links the run it starts as opened where the forge reports its URL. Linked runs
have no completion or cleanup effect; `shoal watch` follows each until it
finishes, and it stays linked until it is unlinked or the workspace is removed.
A PR's runs are not linked; the PR's checks cover them, so link one only to
follow it past the PR's merge. Where
[`[pr] watch_merge_runs`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#prs)
is on, Shoal does that itself once it sees a PR linked as assigned or opened
merge into the default branch, whether or not its agent took the PR's earlier
updates, and once a `shoal merge` of the workspace's branch pushes the default
branch, for the commit it pushed; the background poll reads an opened PR only for this, until it sees
it merge or close, and reports nothing else of it. For ten minutes the background poll lists the default branch's
runs for the merge commit the forge reports, or the one pushed, and links each new one as related,
so the space hears how it ends as of any linked run. Where the forge reports no
merge commit, an assigned PR's merge update says its runs cannot be followed, and a lookup
that finds no run records that in the space's log. Until the lookup ends and the
runs it linked finish, the space stays from every unattended removal, with
`cleanup_reason` `merge_runs`, and its asleep agent counts as one an update
wakes. Commands that select items this way give a number without a
kind the kind of the linked item with that number, otherwise the kind the forge
reports; GitLab and Bitbucket number issues and PRs separately, so there such a
number must be linked. Use `--space` to select another space, a workspace or chat,
than the current one. Numbers resolve against its origin and URLs must match it. Linking
an item again under its relation changes nothing but a newly known title.
Assigned issues accumulate without duplicates in the order they were assigned;
the first names a new branch, gives a new PR its title and starts the agent.
Linking an issue or PR records its title, which later reads of the item on its
forge keep current, and workspace lists show the first assigned issue's; when
the forge cannot be read, the link is recorded without one and a warning names
the error.
An open issue a space links as assigned has the accounts
[`[issue] assign`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#issues) names among its assignees on the forge,
where that account has a token of its own there. With the default, `"agent"`,
the agent account is assigned while an agent works the issue: the space has an
agent of its own that runs, waits in the queue, restarts or is asleep, or an
agent linked the issue and no agent of its own has ended since, and the space is
not done. Shoal takes it off once no
space whose agent works the issue links it, after `shoal done`, a stop that
waits for `shoal resume`, the agent's exit, an unlink or a removal, where this
Shoal put it on, so it leaves one another Shoal acting as the same account
assigned; it puts it back when an agent starts, `shoal resume` queues one,
`shoal done --undo` withdraws the completion or the issue loses an assignee
while an agent here works it. `"user"` adds the user's own login on every assigned
link, whoever makes it, and Shoal takes it off once no space links the issue,
if Shoal added it; `"both"` does both, and `"none"` neither. A closed issue
keeps its assignees. A warning on stderr and in the JSON `warnings` names the
issue's other assignees, or why the forge did not assign it; the link stands
either way. A restored space, or one moved to another machine, assigns its
issues again as before, and takes back the user's login Shoal added there; the
space's JSON lists them under `links.assignees`.
An assignee the forge refuses to change comes back as a warning, or else as an
`unassign_failed` alert, naming the `shoal issue edit` command that
changes it.
Assigned PRs accumulate without duplicates, and any PR of the repository can be
assigned whatever its head branch, such as one pushed from the worktree on
another branch. Assigned items suppress idle cleanup.
A chat links items the same way in its repository's registered checkout. Its
assigned items neither complete nor remove it: only its own `done` records
completion, an assigned issue or a watched PR keeps it from idle cleanup as they
keep a workspace, and its assigned PRs name no branch, so they do not keep a done
chat from completion cleanup; the activity Shoal polls for its items reaches its agent with its
alerts, through the chat's hooks, and wakes it as they do. Its links go with it
when it is removed. A chat started on an issue is assigned the issue.

`shoal view` shows every linked item, with the relation it is linked under, as
the forge reports it now: title, state,
author, labels, assignees, milestone, description, comments, and reviews with their inline comments,
with the reactions on the item and each comment where the forge has them, as
emoji on a terminal in a UTF-8 locale other than the Linux console and by name
otherwise; a
PR adds its branches, its `reviewers` as `pr edit --add-reviewer` names them (on
GitHub only those whose review is still requested, on Forgejo and Gitea also those
who approved or requested changes unasked), how many commits its base has that its head lacks (`behind`,
left out for a closed PR and on Bitbucket), its `auto_merge` while the forge is to
merge it once it can, with the `method` where the forge reports it (Forgejo and
Gitea do not), the merge conflicts, CI checks, runs waiting for approval and
review state `status` reports, plus the last whole lines, up to 8 KiB, of each failed CI job's
log, each line over 1 KiB shortened to its start and end, where the forge
[reads job logs](issues.md#forges), with the job's URL and its failing step when recorded
by the runner or listed by Forgejo 17 and later. The tail is taken after removing terminal escape sequences, text
a carriage return redrew, GitLab's section markers and the timestamps GitHub and
Forgejo start each line with. The daemon fetches logs as the repository's agent
account; successful and pending checks omit log output.
`--check NAME` selects only that failed check's log on one PR, without a number
the assigned one. Unavailable logs
report an error, as does a check that links no CI job in the repository, such as
an outside service's; log downloads are limited to 8 MiB.
Linked runs show as `shoal view ci` shows a run (see [CI runs](ci.md)).
Kinds and explicit items select as they do for `watch`, except that an issue
or PR URL may name another repository's item, shown without failed job logs, and
`--no-comments` leaves out comments and reviews. Outside a workspace or chat, it
shows the item a number or URL names in the repository of the registered checkout
it runs in, or of the one `--repo` or the URL names as for
[issue and PR actions](issues.md#issue-and-pr-actions). Each comment shows its ID; an
inline comment also shows the ID of the comment that starts its thread when it is
a reply, and whether the thread is resolved. A failed lookup reports its error on that item, and a
review whose inline comments could not be read keeps its body; the command exits 1 when an item could not be read. `--json` returns an array
of items with failed logs in `check_logs`, and runs with `kind` `ci`, or a check
log object with `--check`;
`reactions` on an item or comment lists each reaction's `content`, `count`, and
whether the login that read it `reacted`: the daemon reads issues and PRs as the
[agent account](agents.md#agent-accounts) for processes inside a workspace and a chat's
agent, which read only from their space, and with your own logins otherwise. Reactions use GitHub's names (`+1`, `-1`,
`laugh`, `hooray`, `confused`, `heart`, `rocket`, `eyes`); a GitLab emoji
without one keeps its GitLab name.
`--web` opens the selected items in the browser instead, with `open` on macOS
and `xdg-open` elsewhere, without waiting for the browser, and prints each URL,
also when the opener fails; workspace processes cannot.

`shoal watch` polls the assigned issues and PRs and the linked runs every ~30
seconds, and the daemon queues what its own polling of a space's linked items
observes for the next watch, which returns at once: it polls every ready
workspace and every chat of a repository, whether or not its agent runs; `watch pr`, `watch
issue` or `watch ci` filters by kind. A run delivers a `ci_completed` update
each time it finishes, with its final state, `cancelled` for a failed run whose
jobs only stopped by being cancelled, each failed job's failing step where its
log or Forgejo's step list names one, and a pointer to `shoal view ci`. A
finished run stays watched while it is linked, until the space is done, so a
later attempt of it, an automatic retry, `shoal ci rerun` or a rerun on the
forge, reports its finish the same way. `watch <number-or-url>`, `watch pr <number-or-url>` and
`watch issue <number-or-url>` select an explicit item in the workspace's repository
without linking it or changing completion policy; explicit PR watches also accept
PRs whose head branch is in a fork. A [chat without a repository](chats.md#master-session)
watches what is queued for it and the spaces below it, of every kind or the one
selected, and an explicit item in the registered repository its URL names, or for
a number the one `--repo` names. Watches return on comments,
reviews, edits, completed CI checks, new merge conflicts, closure, reopening, merging,
or a merged base workspace, except for comments, review thread replies, edits, closing
and reopening that the workspace itself made through Shoal, on its own items or those of
a space below it whose activity it receives, and the assignees Shoal adds or removes on its
own, which no space's watch reports; the agent of a space below with its own agent
still hears of the others, and the same change made another way, such as in the web interface,
is reported. An `edited` update names what changed of an
item's title, description, labels, assignees, milestone and, for a PR, its draft
state and base branch, such as `Labels added: bug; title changed to "Fix"`;
Shoal's own retargeting of a stacked PR is left to its `base_merged` update. A passing check is held while
another check on the PR's head commit is pending, and arrives with a check that
fails or does not pass, or with the last pending one; a new head commit drops
the passes held for the previous one. `shoal status` and `item_activity` events
show each check as it completes. A PR's conflict is reported once
two polls in a row see it at the same head and base commits, and again when it
persists across new ones.
An unfiltered `shoal watch` without a linked PR also returns when the workspace
branch newly conflicts with the default target of `shoal conflicts`, naming the
branch and the conflicted paths, unless that target already has HEAD's changes as
[removal](removal.md#branches) decides merges; with nothing linked, it waits for that
alone. With a linked
PR, whose forge reports its conflicts, the watch and the agent's deliveries
report only the files [changed on both sides](branches.md#files-changed-on-both-sides), such as
`Changed on both sides with main: src/version.rs`. Once the assignment is done,
its work is finished and the branch is checked no more.
A new [alert](alerts.md) ends a watch at once, ahead of item
activity, even when the watch would otherwise fail for lack of linked items.
Each update includes its item URL, kind and message; a comment update counts the
new, edited and deleted comments and reviews by kind and author. `--json` returns an `updates`
array, and an `alerts` array when alerts arrived. Linking an item, and
an agent starting a new session, take its comments and edits so far as seen, which
`shoal view` shows, while failed checks, conflicts and its state are still reported;
a watch of an item the space does not follow reports its existing activity first.
Watches share a cursor per workspace and item across restarts. The first message of
an agent started on an issue counts the comments the issue already has. Pending updates replay until the
CLI acknowledges successful output. Unlinking discards that item's cursor.
`--timeout <seconds>` bounds the wait to 1–3600 seconds; without it, the wait is
unbounded. A watch survives daemon restarts, waiting up to two minutes for a
daemon that went away to return and know the caller's scope again, or 10 seconds
for one that was not running. Expiration returns an empty array and `timed_out: true`. A newer watch
in the same workspace
supersedes a running one, which returns an empty array and `superseded: true`
without taking updates; updates acknowledged while a newer watch runs stay pending
for it. Scoped callers can watch only in their own workspace and its child
workspaces; a watch also reports the activity its agent receives from
[child workspaces](create.md). Activity polling continues when PR cleanup is disabled. A PR
seen merged is not looked up again, nor, once the assignment is done, an item seen
closed. Linked items are read
from the forge their URLs name, so Git failing in the worktree holds back only the
branch conflict check; a watch of an item named on the command line still reads
the origin remote with Git. A failed lookup of a linked item, or of one of its
sources such as CI, reports a `lookup_failed` update instead of failing the watch,
and the item's last observation stands until a lookup succeeds. A watch of an item
named on the command line fails when that item cannot be read. Each
source's failures are reported at most once every 10 minutes, whatever the error
and however often a poll in between succeeds, and a failure that repeats is
reported once, until a lookup of its source succeeds or fails with another
error. A
forge that refuses a request for its rate limit (`429`, or `403` with
`Retry-After` or no requests remaining), or reports no requests remaining, is
not asked again as that account until its `Retry-After` or reset time, at most
an hour away. Polls wait until then instead of reporting lookup failures, and
the daemon records a `forge_limited` alert. Forgejo
comment and review changes are grouped; CI results are the commit statuses of the
PR's head commit, read with the host's token when it has one. The daemon
repeats a REST read as a conditional request with the `ETag` and `Last-Modified`
of the last response it got for that URL as the same account, and takes a
`304 Not Modified` to mean that response still holds; it keeps them in memory
only. Every forge API read is sent up to three more times, after longer waits each time,
while its connection fails or breaks, it times out, or the forge answers with a
5xx other than a rate limit.
Failed CI jobs include the failing step when the runner records it and a
command to read the log; a log lookup failure stays on that check's update.

Once every linked PR has merged and at least one contains current HEAD, or the
freshly fetched default branch has its changes, as after a rebase that followed
the merge, a workspace marked `done` is cleaned up; PRs are looked up only after `done`. A
confirmed set survives restart without querying again. Cleanup uses normal branch retention and resource release, retaining dirty
or newer work. `inspect` shows lookup and removal
errors in `pr_cleanup`. Invalid registrations retain the workspace.

`shoal unlink <relation>` removes the relation's items; `unlink pr <relation>`,
`unlink issue <relation>` or `unlink ci <relation>` removes one kind, and an explicit number or URL removes
just that item; unlinking fails when it selects nothing linked. Removing the last
link does not mark done or undo recorded completion. [`[cleanup.pr] enabled = false`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#cleanup) pauses PR completion independently of idle
cleanup, globally on reload or per repository immediately; linking and unlinking
remain available.
