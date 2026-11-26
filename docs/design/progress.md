# Progress and ready marks

This file covers ready marks, progress, blockers and the progress section of
draft PRs. The [progress reference](../reference/progress.md) says what
`shoal progress` does, and
[Ready for review](../reference/completion.md#ready-for-review) what
`shoal ready` does.

## Ready marks

### What a mark means

A ready mark is the agent's signal that it needs the user before it can go on.
Either its work is ready for review, or it has a decision or answer to ask
them for, as a plan chat does once it has checked its issue. The shared `work`
rule says so, so every built-in mode marks the same way.

A mark is a status signal for integrations, apart from messages and
completion. It never notifies, records completion or changes cleanup.

### What a mark covers

A mark covers a linked issue or PR, or the space when nothing is linked.

A mark carries no commit. It waits for the user, not for a revision, and a
chat's checkout rarely moves. So new commits leave a mark as it was. It lasts
until it is withdrawn, its item is unlinked or the space goes.

Only an open PR whose head on the forge is HEAD takes a mark. A branch that
opens several PRs in turn leaves the merged ones linked, and nobody can review
a PR at a commit it lacks. Marking skips the other PRs with their reasons
instead of failing the rest.

### Marks and draft PRs

Marking a linked draft PR takes it out of draft by default, since forges and
review bots often skip drafts. Withdrawing a mark leaves the PR ready by
default. An agent usually withdraws a mark to push more work that still
needs CI and review.

A failed forge change fails the command and keeps the marks, so the agent can
retry it.

### Mark events and the ready hook

Each mark and withdrawal is a journal event. `hooks.post_ready` runs after
each explicit mark, under the rules of
[the completion hook](cleanup.md#the-completion-hook). A failed hook notifies
and keeps the marks.

### Marks in chats

A chat marks as a workspace does, except that its checkout is not on a PR's
branch. So it marks each linked open PR whatever its head on the forge. Its
marks show it `ready` in lists and the inbox.

### Marks while the agent works

A working agent never waits for the user. So its own `working` state outranks
its space's `ready`, as `needs input` does, and the inbox leaves out the marks
of a space whose agent works.

The mark stays recorded and shows again when the turn ends. A space thus shows
no `ready` while its agent works on the user's reply, and the agent need not
mark itself again.

## Progress

### What progress is

Progress is where a space's agent is in its work, beside its turn. It is a
status line in the agent's own words and the steps of its plan.

Progress outlives the agent until the space goes, so a finished space shows
where its work ended. Like [agent state](agents.md#agent-state), it never
notifies. Coordinators read it on demand.

### Steps from plan tools

Agents already keep their steps in their plan tools. So Shoal reads the steps
through `PostToolUse` hooks instead of asking agents to report them.

Each integration only turns its hook event into a change, and the daemon keeps
the list. A tool that writes the whole plan replaces the steps. A task tool
changes one task by its ID. Agents without a plan hook set the steps from a
task list.

### Progress events

Each change of progress is a journal event of the space. The journal keeps
them as it keeps turn states, since plan tools change progress as often.

## Blockers

### What a blocker is

A blocker is part of progress: it says why the work does not move. It sits on
the space or on one step. It names a space, an issue, a PR or a CI run, with a
one-line reason.

Threads and approval requests already cover waiting for a person, so nothing
else blocks.

### How agents set blockers

The hooks of plan tools cannot carry blockers, so agents set them with
`shoal progress`. A step's blocker follows the step by its tool's key, or by
its name when the tool gives none. The blocker goes when the step goes.

A space is blocked while it has a blocker of its own, or while every open step
has one.

### When a blocker clears

One resolver serves [queued launches](queue.md) and spaces. Each poll looks
up each forge blocker once, and each blocking run's workflow on the default
branch once. A space blocker clears as admission decides it for a launch.
[When a blocker clears](../reference/progress.md#when-a-blocker-clears) says
what clears each kind.

A CI run blocker also clears once a later run of its workflow on the default
branch passes. A merge that fixes a broken default branch thus releases every
agent it blocked, without anyone touching them.

### Blocked notices

Becoming blocked is the one blocker event that notifies. It goes the way a
`done` notice goes, so a coordinator learns of it once.

### A blocked agent

A blocked agent has nothing to do. So once its turn ends, Shoal stops it as an
[idle stop](agent-lifecycle.md#idle-sleep) does. Its restore waits until
nothing blocks the space, instead of for an update. A chat comes back the same
way, through its automatic restore.

`shoal done` removes the blockers and cancels the restore.

## Progress in draft PRs

### The section belongs to Shoal

With `[pr] progress` on, an open draft PR the space opened or was assigned
shows the space's progress. The progress fills a section at the end of the
PR's body, which belongs to Shoal.

The daemon writes the section through the forge edits it makes for the
space's other changes. It records each edit as its own, so the watch does not
report it back. It limits how often it edits a PR and always writes the
latest progress in the end, as
[Progress in draft PRs](../reference/progress.md#progress-in-draft-prs) says.

### The section follows the draft state

The section follows the PR's draft state, not its ready mark. A draft is the
work in progress. Reviewers read a PR out of draft, and they read its
description, not a live plan.

Each update reads the PR. It writes the section to a draft and removes it from
any other PR. So a PR takes the same path whether it leaves draft through
`shoal ready`, `shoal pr edit` or the forge, which Shoal's own edits and the
background poll report.

### Why it is opt-in

A status line written for `shoal list` reads as noise in a description written
for reviewers. So only someone who follows agents on the forge, and opens their
PRs as drafts, turns it on.
