# Progress

This file says what `shoal progress` does.

```sh
shoal progress "Reproduced; the cause is the 5-second query limit"  # Set the status line ("" clears it)
shoal progress                                                     # Show the status line and steps
shoal progress --space 4                                           # Another workspace or chat
shoal progress --steps < plan.md                                   # Replace the steps (empty input clears them)
shoal progress --add-blocker 1468 --reason "CI runner is down"     # Record what blocks the space
shoal progress --add-blocker <id> --step 3 --reason "Needs the API" # Record what blocks step 3
shoal progress --remove-blocker 1468 [--step 3]                    # Remove one
shoal done --summary "Fix merged; the timeout is now 30 seconds"   # Completes with this as the status line
```

Each workspace and chat keeps its agent's progress: a status line in the
agent's own words and the steps of its plan, each done or not. The status line
and each step name are a single line of at most 512 bytes without control
characters, as an alert's. A plan keeps its first 50 steps. The current step
is the one the agent's plan tool marks in progress, else the first that is not
done. Progress lasts until it is replaced or the space is removed, across
daemon restarts and agent exits, so a completed space still shows where its
work ended: a `shoal done --summary` sets the status line with the
completion, and `shoal done --undo` leaves it as it is.

Claude Code's and Codex's plan tools set the steps through `PostToolUse` hooks
Shoal registers with the [alert hooks](alerts.md), which run
`shoal internal plan --integration <name>` with the hook event on stdin:
`TodoWrite` and `update_plan` replace the steps, and `TaskCreate` and
`TaskUpdate` add and change one task by its ID, where creating an ID the plan
already has starts a new plan. A subagent's calls change nothing, and Happy
sessions register no hooks. Other agents set the steps with `--steps`, which
reads a markdown task list, one `- [ ] name` or `- [x] name` per line.

A blocker records what blocks the space, or one step of its plan with
`--step`, numbered as `shoal progress` shows the steps, and why, in a
`--reason` of one line. `--add-blocker` takes an issue by number or URL, so a
bare number is an issue, never a space's number, a PR or CI run by URL, or a
space, a workspace or a chat, by its ID; adding it again replaces its
reason. `--remove-blocker` names one the same way, and a space's coordinator
removes one of a space below with `--space`. A step's blocker follows the
step by the key its plan tool gave it, or by name when it has none, and goes
with the step when the plan drops it. A space is blocked while it has a
blocker of its own, or while every step that is not done has one. Adding and
removing a blocker are `blocker_added` and `blocker_removed` events, which
`shoal log` shows. When a space becomes blocked, Shoal tells its
coordinator, or the user when it has none, once, as a `space_blocked`
alert naming the space and what blocks it, as it reports `shoal done`;
nothing else about blockers notifies, and a space notifies again only once it
becomes blocked again after it cleared.

Every 30 seconds Shoal removes the blockers that cleared, each a
`blocker_cleared` event naming what cleared it, and tells the space's agent
what cleared and what still blocks it. An issue clears once it closes or a
workspace of the space's repository that links it is done, a PR once it
merges or closes, a space once it is done at its current HEAD or removed, and
a CI run once it passes, as after a rerun, or once the newest finished run of
its workflow on the default branch passes and started after it, whatever
branch the blocking run was on, so each merge into the default branch is a new
chance to clear it. One lookup of each workflow serves every blocker on it. A
failed lookup keeps the blocker and shows why as its `lookup_failed` until a
lookup succeeds.

Once a blocked space's agent ends a turn, with no work of its own still
running, Shoal stops it as an [idle stop](sleep.md#idle-sleep) does, without waiting for
you, keeps its session and queues its restore, which waits until
nothing blocks the space and lists what blocks it in `shoal queue`; updates
for the agent wait with it. The restored agent's first message is the one
naming what cleared. `shoal resume` restores the agent at once, telling it
what still blocks the space; `shoal stop` cancels the waiting restore as it
cancels other queued restores, and the blockers stay. `shoal done` removes
the space's blockers and cancels the restore. The waiting restore keeps the
space from cleanup, as other automatic restores do. A chat's agent stops the
same way: Shoal saves the chat to come back on its own, which waits while it
is blocked, then restores it in a background place, as `shoal restart` does,
provided its session can be resumed and no work it started still runs
outside its agent's process group; `shoal done` on such a chat records its
completion and keeps it stopped for `shoal resume`. A restored chat with
alerts waiting starts its first turn with the prompt that delivers them, as
a workspace agent's wrapper wakes it.

`status` shows a `Progress` section with the status line and the numbered
steps, marking the current one, and `list` shows how many steps are done with
the current step, such as `3/5 Fix the timeout`, followed by the status line
in place of the issue title where the width allows. `status`, `inspect`,
`list --json` and `shoal progress --json` give `progress` with `status`,
`steps` (`name`, `done`, `blockers`), `current` (the step's index, or null
once every step is done), `blockers` and Unix-seconds `updated_at`, each
blocker with its `kind` (`space`, `issue`, `pr` or `ci_run`), `target` (the
space's ID or the item's URL), `name`, `reason` and, after a failed lookup,
`lookup_failed`; a space without progress has none. `shoal progress` and the
`Progress` section list each blocker with its cause and reason, the space's
after the status line and a step's below it.
Each change is a `progress_changed` [event](log.md#space-events), which
`shoal log --all` shows, and setting the same value again is not a
change. Progress never notifies, and coordinating agents read their
children's progress on demand.

With [`[pr] progress = true`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#prs), while a space has an open draft
PR it opened or was assigned, Shoal writes the status line and the steps into
a section at the end of the PR's body, as the agent account:

```markdown
<!-- shoal:progress -->
**Status:** Reproduced; the cause is the 5-second query limit

- [x] Reproduce
- [ ] Fix the timeout
<!-- /shoal:progress -->
```

The section is Shoal's: each update replaces what is between the markers and
leaves the rest of the body as it is. A PR is edited at most once a minute,
and the latest progress is always written in the end, including what changed
while the daemon was down; a PR opened later shows it with the next change. A
failed edit is logged and retried with the next change, without a
alert. A PR that leaves draft, through
[`shoal ready`](completion.md#ready-for-review), `shoal pr edit --no-draft` or the forge,
loses the section with the next publication, and one that returns to draft
shows it again. Merged and closed PRs are never edited, and completing with
`shoal done` keeps the section. Turning `[pr] progress` off stops the updates
and leaves existing sections as they are.

Scoped callers set only their own space's progress, and read their own and
that of the spaces below it.
