# Links and watches

This file says what `shoal link`, `unlink`, `view` and `watch` do, and how
linked items affect a space. [Links and watches](../design/links.md) gives the
reasons.

## Link an item

```sh
shoal link 42 assigned                  # an issue or PR, by number
shoal link pr 12 opened                 # only a PR
shoal link issue related                # pick an open issue on a terminal
shoal link ci https://forge.example/team/repo/actions/runs/2765 related
shoal link 42 assigned --space 5        # link to another space
```

`shoal link` links an issue, PR or CI run to a space under a relation. The
space is the workspace the command runs in, else the chat, or the one
`--space` names.

`link issue`, `link pr` and `link ci` name the kind. A run's URL alone also
links a run. A number names a run only after `ci`.

Without an item, a terminal offers the space's open issues or PRs in a picker,
asking for the kind first when none is given. Without a terminal the command
is refused, and a run always needs its number or URL.

### Select items

`link`, `unlink`, `view` and `watch` select items the same way.

A number without a kind takes the kind of the linked item with that number.
When none is linked, it takes the kind the forge reports. GitLab and Bitbucket
number issues and PRs separately, so there such a number must name a linked
item.

A number resolves against the space's `origin`. `link`, `unlink` and `watch`
take an issue's or PR's URL only of that repository. A run's URL may name
another repository, where [CI](ci.md#another-repository) says who may act.

### Relations

The relation is required:

| Relation | Effect |
|---|---|
| `assigned` | The work the space exists for. The rules below and in [ready for review](completion.md#ready-for-review) apply. |
| `opened` | Records that the space created the item. `issue open` and `pr open` record what they create this way and do not assign it. |
| `related` | Records an item the work relates to. |

A space links each item under one relation:

- Assigning an item replaces its opened or related link.
- Linking an opened item as related replaces that link, and the other way
  round.
- An assigned item refuses another relation until it is unlinked.

Linking an item again under its relation changes nothing but a newly known
title.

Opened and related issues and PRs have no watch, completion or cleanup effect.
[`shoal status`](workspaces.md#find-the-spaces-of-an-item) finds the spaces
that link an item under any relation.

### Titles

Linking an issue or PR records its title. Later reads of the item on its forge
keep the title current. Workspace lists show the first assigned issue's title.

When the forge cannot be read, Shoal records the link without a title, and a
warning names the error.

### Assigned issues and PRs

Assigned issues accumulate without duplicates, in the order they were
assigned. The first names a new branch, gives a new PR its title and starts the
agent.

Assigned PRs accumulate without duplicates. Any PR of the repository can be
assigned, whatever its head branch, such as one pushed from the worktree on
another branch. A done workspace waits for its assigned PRs, as
[cleanup after `done`](completion.md#cleanup-after-done) says.

Assigned items keep a space from
[idle cleanup](removal.md#automatic-cleanup).

### Assignees on the forge

An open issue a space links as assigned gets the accounts that
[`[issue] assign`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#issues)
names among its assignees on the forge. Each account needs a token of its own
on that forge.

With the default, `"agent"`, the agent account is assigned while an agent works
the issue. An agent works the issue while the space is not done and either:

- the space's own agent runs, waits in the queue, restarts or is asleep, or
- an agent linked the issue, and no agent of the space's own has ended since.

Shoal takes the agent account off once no space whose agent works the issue
links it, as after `shoal done`, a stop that waits for `shoal resume`, the
agent's exit, an unlink or a removal. It takes off only an assignment this
Shoal made, so another Shoal acting as the same account keeps its own.

Shoal puts the agent account back when:

- an agent starts,
- `shoal resume` queues one,
- `shoal done --undo` withdraws the completion, or
- the issue loses an assignee while an agent here works it.

`"user"` adds your own login on every assigned link, whoever makes it. Shoal
takes it off once no space links the issue, if Shoal added it. `"both"` does
both, and `"none"` neither.

A closed issue keeps its assignees.

A warning on stderr and in the JSON `warnings` names the issue's other
assignees, or why the forge did not assign it. The link stands either way.

An assignee the forge refuses to change comes back as a warning, or else as an
`unassign_failed` alert. Both name the `shoal issue edit` command that changes
it.

A restored space, or one moved to another machine, assigns its issues again as
before. It also takes back the user's login that Shoal added there. The space's
JSON lists these assignments under `links.assignees`.

### CI runs

A run is linked only as opened or related. Its workflow is its title.
[`shoal ci run`](ci.md#start-a-run) links the run it starts as opened.

`shoal watch` follows a linked run until it finishes. The run stays linked
until it is unlinked or the space is removed. A linked run has no completion or
cleanup effect of its own; the merge runs below keep a space.

A PR's runs are not linked, since the PR's checks cover them. Link one only to
follow it past the PR's merge.

### Merge runs

Where
[`[pr] watch_merge_runs`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#prs)
is on, Shoal links the default branch's runs for a space's merges itself. It
starts once it sees either:

- a PR linked as assigned or opened merge into the default branch, whether or
  not its agent took the PR's earlier updates, or
- a `shoal merge` of the workspace's branch push the default branch, for the
  commit it pushed, as [push after merging](branches.md#push-after-merging)
  says.

The background poll reads an opened PR only for this. It stops once it sees
the PR merge or close, and reports nothing else of it.

For ten minutes, the background poll lists the default branch's runs for the
merge commit the forge reports, or the commit pushed. It links each new run as
related, so the space hears how the run ends, as for any linked run.

Where the forge reports no merge commit, an assigned PR's merge update says
its runs cannot be followed. A lookup that finds no run records that in the
space's log.

Until the lookup ends and the runs it linked finish, the space stays from every
unattended removal, with `cleanup_reason` `merge_runs`. Meanwhile its asleep
agent counts as one an update wakes.

### Chats

A chat links items the same way, in its repository's registered checkout. A
chat started on an issue is assigned the issue.

A chat's assigned items neither complete nor remove it. Only its own
[`done`](completion.md#done-chats) records completion.
[Removal](removal.md#chats) says which of its links keep it from idle cleanup.

The activity Shoal polls for a chat's items reaches its agent with its alerts,
through the chat's hooks, and wakes it as they do. A chat's links go with it
when it is removed.

## View items

```sh
shoal view                     # every linked item
shoal view pr 12               # one PR
shoal view --check rust        # the log of the assigned PR's failed check rust
shoal view --no-comments
shoal view --web               # open the items in the browser
```

`shoal view` shows every linked item as the forge reports it now, with the
relation it is linked under. A kind or an item narrows it, as in
[Select items](#select-items). An issue's or PR's URL may also name another
repository's item, which shows without failed job logs. `shoal view ci` shows a
run, as [CI](ci.md#view-a-run) says, and linked runs show the same way.

Outside a workspace or chat, `view` shows the item a number or URL names. It
reads the repository of the registered checkout it runs in, or the one `--repo`
or the URL names, as for [another repository](issues.md#another-repository).

[Agent accounts](agents.md#agent-accounts) say which login the daemon reads
with.

### What a view shows

Each issue and PR shows its title, state, author, labels, assignees,
milestone, description, comments, and reviews with their inline comments.

A PR adds:

- its branches;
- its `reviewers`, named as `pr edit --add-reviewer` takes them: on GitHub only
  those whose review is still requested, and on Forgejo and Gitea also those
  who approved or requested changes unasked;
- `behind`, how many commits its base has that its head lacks, left out for a
  closed PR and on Bitbucket;
- its `auto_merge` while the forge is to merge it once it can, with the
  `method` where the forge reports it, which Forgejo and Gitea do not;
- the merge conflicts, CI checks, runs waiting for approval and review state
  that `status` reports;
- the log tails of failed CI jobs, as below.

`--no-comments` leaves out comments and reviews.

### Comments and reactions

Each comment shows its ID. An inline comment that is a reply also shows the ID
of the comment that starts its thread. An inline comment shows whether its
thread is resolved.

The item and each comment show their reactions where the forge has them. A
terminal in a UTF-8 locale shows them as emoji, except the Linux console, and
anything else by name. Reactions take GitHub's names: `+1`, `-1`, `laugh`,
`hooray`, `confused`, `heart`, `rocket` and `eyes`. A GitLab emoji without one
keeps its GitLab name.

### Failed job logs

Each failed CI job shows the last whole lines of its log, up to 8 KiB, where
the forge [reads job logs](issues.md#capabilities). A line over 1 KiB is shortened to
its start and end. Shoal takes the tail after removing:

- terminal escape sequences,
- text a carriage return redrew,
- GitLab's section markers,
- the timestamps that GitHub and Forgejo start each line with.

The tail comes with the job's URL, and with its failing step where the runner
records it or Forgejo 17 and later lists it. Successful and pending checks show
no log. The daemon fetches logs as the repository's agent account and
downloads at most 8 MiB.

`--check NAME` shows only that failed check's log, on one PR: the one given, or
else the assigned one. An unavailable log reports an error, and so does a check
that links no CI job in the repository, such as an outside service's.

### Failures and JSON

A failed lookup reports its error on that item. A review whose inline comments
could not be read keeps its body. The command exits 1 when an item could not be
read.

`--json` returns an array of items, with failed logs in `check_logs` and runs
with `kind` `ci`. With `--check` it returns a check log object. `reactions` on
an item or comment lists each reaction's `content` and `count`, and whether the
login that read it `reacted`.

### Open in the browser

`--web` opens the selected items in the browser instead and prints each URL.
It uses `open` on macOS and `xdg-open` elsewhere, and does not wait for the
browser. It prints the URLs also when the opener fails. Workspace processes
cannot use it.

## Watch items

```sh
shoal watch                     # assigned issues and PRs, linked runs
shoal watch pr                  # only PRs
shoal watch pr 12               # one PR, without linking it
shoal watch --timeout 600
```

`shoal watch` waits for activity on the space's assigned issues and PRs and its
linked runs, and returns it. `watch pr`, `watch issue` and `watch ci` keep one
kind.

`watch <number-or-url>`, `watch pr <number-or-url>` and
`watch issue <number-or-url>` watch an item in the workspace's repository
without linking it or changing completion. A PR named this way may have its
head branch in a fork.

A [chat without a repository](chats.md#master-session) watches what is queued
for it and the spaces below it, of every kind or the one given. It also watches
an item named by its URL in that registered repository, or for a number in the
one `--repo` names.

### Polling

The daemon polls the linked items of every ready workspace, and of every chat
of a repository, about every 30 seconds, whether or not its agent runs. It
queues what it observes for the next watch, which then returns at once.

The daemon reads linked items from the forge their URLs name. Git failing in
the worktree therefore holds back only the branch conflict check. A watch of an
item named on the command line still reads the `origin` remote with Git.

Polling goes on when PR cleanup is disabled. A PR seen merged is not looked up
again. Once the assignment is done, an item seen closed is not looked up
again either.

On Forgejo, comment and review changes are grouped. A PR's CI results are the
commit statuses of its head commit, read with the host's token when it has
one.

### What a watch reports

A watch returns on:

- comments, reviews and edits,
- completed CI checks,
- new merge conflicts,
- closing, reopening and merging,
- a merged base workspace.

Each update has its item's URL, its kind and a message. A comment update counts
the new, edited and deleted comments and reviews by kind and author.

An `edited` update names what changed of the item's title, description,
labels, assignees and milestone, and of a PR's draft state and base branch,
such as `Labels added: bug; title changed to "Fix"`. Shoal's own retargeting of
a stacked PR arrives as its `base_merged` update instead.

A failed check's update names its failing step where the runner records it,
and a command to read the log. A failed log lookup stays on that check's
update.

`shoal status` and `item_activity` events show each check as it completes.

### Changes a space made itself

A watch leaves out the comments, review thread replies, edits, closing and
reopening the space made itself through Shoal. That covers its own items and
those of a space below whose activity it receives. The agent of a space below
that has its own agent still hears of them.

No watch reports the assignees Shoal adds or removes on its own.

The same change made another way, such as in the web interface, is reported.

### Held checks

A passing check waits while another check on the PR's head commit is pending.
It arrives with a check that fails or does not pass, or with the last pending
one. A new head commit drops the passes held for the previous one.

### Merge conflicts

A PR's conflict is reported once two polls in a row see it at the same head and
base commits. It is reported again when it persists across new ones.

An unfiltered watch without a linked PR also returns when the workspace's
branch newly conflicts with the default target of
[`shoal conflicts`](branches.md#conflicts). The update names the branch and the
conflicting paths. It leaves out a target that already has HEAD's changes, as
[removal](removal.md#branches) decides merges. With nothing linked, the watch
waits for that alone.

With a linked PR, the forge reports the conflicts. The watch and the agent's
deliveries then report only the
[files changed on both sides](branches.md#files-changed-on-both-sides), such as
`Changed on both sides with main: src/version.rs`.

Once the assignment is done, the work is finished and Shoal checks the branch
no more.

### CI run updates

A run delivers a `ci_completed` update each time it finishes, with:

- its final state, `cancelled` for a failed run whose jobs stopped only by
  being cancelled;
- each failed job's failing step, where its log or Forgejo's step list names
  one;
- a pointer to `shoal view ci`.

A finished run stays watched while it is linked, until the space is done. A
later attempt of it, such as an automatic retry, `shoal ci rerun` or a rerun on
the forge, reports its finish the same way.

### Alerts

A new [alert](alerts.md) ends a watch at once, ahead of item activity. It does
so even when the watch would otherwise fail for lack of linked items. `--json`
returns an `updates` array, and an `alerts` array when alerts arrived.

### What a watch has seen

Watches share a cursor per workspace and item, which survives restarts.

Linking an item takes its comments and edits so far as seen, and so does an
agent starting a new session. `shoal view` shows them. Failed checks, conflicts
and the item's state are still reported. The first message of an agent started
on an issue counts the comments the issue already has.

A watch of an item the space does not follow reports its existing activity
first.

Pending updates replay until the CLI acknowledges successful output. Unlinking
an item discards its cursor.

### Waiting

`--timeout <seconds>` bounds the wait to 1–3600 seconds. Without it, the wait
is unbounded. When the timeout expires, the watch returns an empty array and
`timed_out: true`.

A newer watch in the same workspace supersedes a running one. The running one
returns an empty array and `superseded: true`, without taking updates. Updates
acknowledged while a newer watch runs stay pending for it.

A watch survives daemon restarts. It waits up to two minutes for a daemon that
went away to return and know the caller's scope again, and 10 seconds for one
that was not running.

Scoped callers watch only in their own workspace and its child workspaces. A
watch also reports the activity its agent receives from
[child workspaces](create.md).

### Failed lookups

A failed lookup of a linked item, or of one of its sources such as CI, reports
a `lookup_failed` update instead of failing the watch. The item's last
observation stands until a lookup succeeds. A watch of an item named on the
command line fails when that item cannot be read.

Each source's failures are reported at most once every 10 minutes, whatever the
error and however often a poll in between succeeds. A failure that repeats is
reported once, until a lookup of its source succeeds or fails with another
error.

A forge's [rate limit](issues.md#rate-limits-and-retries) makes polls wait
instead of reporting lookup failures.

## Unlink items

```sh
shoal unlink assigned           # every assigned item
shoal unlink pr assigned        # every assigned PR
shoal unlink ci 2765 related    # one run
```

`shoal unlink <relation>` removes the space's items of that relation.
`unlink issue`, `unlink pr` and `unlink ci` remove one kind, and a number or
URL removes one item. Unlinking fails when it selects nothing linked.

Removing the last link neither marks the space done nor withdraws a recorded
completion.
