# Progress and ready marks

This file covers ready marks, progress, blockers and the progress section of
draft PRs.

Ready-for-review marks are the agent's signal that it needs the user before
it can go on, in every space: its work is ready for review, or it has a
decision or answer to ask them for, as a plan chat does once it has checked
its issue. The shared `work` rule says so, so every built-in mode marks the
same way. They are a status signal for integrations, separate from messages
and completion: they never notify, complete, or change cleanup. A
mark covers a linked issue or PR, or the workspace when nothing is linked, and
carries no commit: what it waits for is the user, not a revision, and a chat's
checkout rarely moves, so it lasts until `shoal ready --undo`, unlinking its
item or removing the space, new commits leave it as it was, and marking again
refreshes it. Only an open PR whose
head on the forge is HEAD takes a mark, since a branch that opens several PRs in
turn leaves merged ones linked, and a PR cannot be reviewed at a commit it lacks;
marking skips the others with their reasons rather than failing the rest. Marking linked items
replaces a workspace mark. Unlinking an item
withdraws its mark and removal deletes all marks. Marks and withdrawals are
journal events, and `hooks.post_ready` runs after each explicit mark under the
completion hook rules; failure notifies and keeps the marks. Marking a linked
draft PR also takes it out of draft unless `[pr] undraft_on_ready = false`, since
forges and review bots often skip drafts. Withdrawing a mark leaves the PR
ready unless `--draft` or `[pr] draft_on_unready` asks to return it to draft, because
an agent usually withdraws to push more work that still needs CI and review.
A failed forge change fails the command with the marks kept, so the agent can
retry it.
A chat marks the same way, except that its checkout is not on a PR's branch:
it marks each linked open PR whatever its head on the forge, and its issue or
itself. Its marks are journal events as a workspace's are,
and they show it `ready` in lists and the inbox.
A working agent never waits for the user, so its own `working` state outranks
its space's `ready`, as `needs input` does, and the inbox leaves out the marks
of a space whose agent works: the mark stays recorded and shows again when the
turn ends, so a space keeps no `ready` status while its agent works on the
user's reply, and the agent need not mark itself again.

Progress is where a space's agent is in its work, beside its turn: a status
line in the agent's own words and the steps of its plan. Agents already keep
their steps in their plan tools, so Shoal reads them through `PostToolUse`
hooks instead of asking agents to report them: each integration only turns its
hook event into a change, whole-plan tools replacing the steps and task tools
changing one task by its ID, and the daemon keeps the list. Agents without a
plan hook set the steps from a task list. Progress outlives the agent until the
space goes, so a finished space shows where its work ended, and like agent state it
never notifies; coordinators read it on demand. Each change is a space's
journal event, kept as its turn states are since plan tools change it as
often.

Blockers are part of progress, since they say why the work does not move:
each sits on the space or on one step and names a space, an issue, a PR or a
CI run with a one-line reason. Threads and approval requests already cover
waiting for a person, so nothing else blocks. Plan tools set steps through
hooks that cannot carry blockers, so agents set them with `shoal progress`
flags, and a step's blocker follows the step by its tool's key, or its name
without one, and goes with it. A space is blocked while a blocker of its own
or one on every open step remains. One resolver serves queued launches and
spaces: the item poll looks each forge blocker and each blocking run's
workflow on the default branch up once, and a space blocker clears as
admission decides for a launch. An issue blocker clears once the issue
closes, and a PR blocker once the PR merges or closes. A CI run blocker clears
once it passes, or a later run of its workflow on the default branch passes,
so a broken default branch that a merge fixes releases every agent it blocked
without anyone touching them.
Becoming blocked is the one blocker event that notifies, through the path
that reports `done`, so a coordinator learns of it once. A blocked agent has
nothing to do, so once its turn ends Shoal stops it as an idle stop does and
its restore waits until nothing blocks the space instead of for an update;
`shoal done` removes the blockers and cancels it. A chat comes back the same
way through its automatic restore.

With `[pr] progress` on, an open draft PR the space opened or was assigned
shows its progress in a section at the end of its body, which belongs to
Shoal: the daemon writes it through the same forge-edit path as the space's
other changes, recorded as its own so the watch does not report it back, at
most once a minute per space with the latest progress written in the end. The section follows the PR's draft
state, not the ready mark: a draft is the work in progress, while a PR out of
draft is read by reviewers, who read the description, not a live plan. Each
publication reads the PR and writes the section only to a draft, removing it
otherwise, so leaving draft through `shoal ready`, `shoal pr edit` or the
forge, which Shoal's own edits and the background poll report, takes one path.
It is opt-in: a status line written for `shoal list` reads as noise in a
description written for reviewers, so only someone following agents on the
forge, who opens their PRs as drafts, turns it on.
