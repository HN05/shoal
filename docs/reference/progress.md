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
```

[Progress](../design/progress.md) in the design says why Shoal keeps
progress and how blockers work.

## Status line and steps

Each workspace and chat keeps its agent's progress. Progress is a status line
in the agent's own words and the steps of its plan, each done or not.

- The status line and each step name are one line of at most 512 bytes,
  without control characters, as an alert's text.
- A plan keeps its first 50 steps.
- The current step is the one the agent's plan tool marks in progress. Without
  one, it is the first step that is not done.

Progress lasts until it is replaced or the space is removed. It survives
daemon restarts and agent exits, so a completed space still shows where its
work ended. [`shoal done --summary`](completion.md#assignment-completion)
sets the status line.

## Steps from plan tools

Claude Code's and Codex's plan tools set the steps through `PostToolUse` hooks.
Shoal registers these hooks with the [alert hooks](alerts.md). They run
`shoal internal plan --integration <name>` with the hook event on stdin.

- `TodoWrite` and `update_plan` replace the steps.
- `TaskCreate` and `TaskUpdate` add or change one task by its ID. Creating an
  ID the plan already has starts a new plan.

A subagent's plan tool changes nothing. Happy sessions register no hooks.

Other agents set the steps with `--steps`. It reads a markdown task list, one
`- [ ] name` or `- [x] name` per line.

## Blockers

A blocker records what blocks the space, and why. `--step` puts it on one
step instead, numbered as `shoal progress` shows the steps. `--reason` takes
one line.

`--add-blocker` takes one of these:

- an issue, by number or URL. A bare number is always an issue, never a
  space's number.
- a PR or a CI run, by URL.
- a space, a workspace or a chat, by its ID.

Adding a blocker again replaces its reason. `--remove-blocker` names a blocker
the same way. A coordinator removes a blocker of a space below with `--space`.

A step's blocker follows the step by the key its plan tool gave it, or by its
name when it has none. The blocker goes when the plan drops the step.

A space is blocked while it has a blocker of its own, or while every step that
is not done has one.

`shoal log` shows each added and removed blocker as a `blocker_added` or
`blocker_removed` event. When a space becomes blocked, Shoal sends its
coordinator, or else the user, a `space_blocked` alert. The alert names the
space and what blocks it, and goes the way `shoal done`'s notice goes. A space
alerts again only after it clears and becomes blocked again. Nothing else about
blockers notifies.

### When a blocker clears

Shoal checks blockers every 30 seconds. A blocker clears once:

- an issue closes, or a workspace of the space's repository that links it is
  done.
- a PR merges or closes.
- a space is done at its current HEAD, or removed.
- a CI run passes, as after a rerun. It also clears once the newest finished
  run of its workflow on the default branch passes and started after it,
  whatever branch the blocking run was on.

Shoal removes each cleared blocker as a `blocker_cleared` event that names what
cleared it. It tells the space's agent what cleared and what still blocks the
space. A failed lookup keeps the blocker and shows why as its `lookup_failed`
until a lookup succeeds.

### A blocked agent

Once a blocked space's agent ends a turn, with no work of its own still
running, Shoal stops it as an [idle stop](sleep.md#idle-sleep) does. Shoal
does not wait for you first. It keeps the agent's session and queues its
restore.

- The restore waits until nothing blocks the space. `shoal queue` lists it
  with what blocks it.
- Updates for the agent wait with the restore.
- The restored agent's first message names what cleared.
- The waiting restore keeps the space from cleanup, as other automatic
  restores do.

`shoal resume` restores the agent at once and tells it what still blocks the
space. `shoal stop` cancels the waiting restore, as it cancels other queued
restores, and keeps the blockers. `shoal done` removes the space's blockers and
cancels the restore.

A chat's agent stops the same way. Shoal restores the chat in a background
place once nothing blocks it, as `shoal restart` does. That needs a session
that can be resumed, and no work the chat started still running outside its
agent's process group. A restored chat with alerts waiting starts its first
turn with the prompt that delivers them, as a workspace agent's wrapper does.
`shoal done` on a blocked chat records its completion and keeps it stopped for
`shoal resume`.

## Show progress

`shoal progress` and `shoal status` list the status line, the numbered steps
with the current one marked, and each blocker with its cause and reason. The
space's blockers follow the status line, and a step's blockers follow the step.
`status` shows them in a `Progress` section.

`shoal list` shows how many steps are done and the current step, such as
`3/5 Fix the timeout`. The status line follows, in place of the issue title,
where the width allows.

`shoal progress --json`, `status --json` and `list --json` give `progress`,
which a space without progress lacks:

- `status`, the status line.
- `steps`, each with `name`, `done` and `blockers`.
- `current`, the current step's index, or null once every step is done.
- `blockers`, the space's own.
- `updated_at`, in Unix seconds.

Each blocker has its `kind` (`space`, `issue`, `pr` or `ci_run`), its
`target` (the space's ID or the item's URL), `name` and `reason`. After a
failed lookup it also has `lookup_failed`.

Each change is a `progress_changed` [event](log.md#space-events), which
`shoal log --all` shows. Setting the same value again is no change. Progress
never notifies: coordinating agents read their children's progress on demand.

## Progress in draft PRs

With [`[pr] progress = true`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#prs),
Shoal writes a space's progress into its open draft PRs: those it opened or
was assigned. Shoal writes the status line and the steps, as the agent
account, into a section at the end of the PR's body:

```markdown
<!-- shoal:progress -->
**Status:** Reproduced; the cause is the 5-second query limit

- [x] Reproduce
- [ ] Fix the timeout
<!-- /shoal:progress -->
```

Each update replaces what lies between the markers and leaves the rest of the
body as it is.

- Shoal edits a PR at most once a minute. The latest progress is always
  written in the end, including what changed while the daemon was down.
- A PR opened later shows the progress with the next change.
- A failed edit is logged and retried with the next change. It sends no alert.
- A PR that leaves draft loses the section with the next update, whether
  through [`shoal ready`](completion.md#ready-for-review),
  `shoal pr edit --no-draft` or the forge. A PR that returns to draft shows it
  again.
- Shoal never edits a merged or closed PR. `shoal done` keeps the section.

Turning `[pr] progress` off stops the updates and leaves existing sections as
they are.

## Scope

A scoped caller sets only its own space's progress. It reads its own progress
and that of the spaces below it.
