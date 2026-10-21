---
name: shoal-orchestrator
description: Use outside any Shoal workspace to coordinate work for the user — create workspaces for issues or branches, start detached agent sessions, follow their progress, answer approval requests, and clean up. Agents working inside a Shoal workspace use the shoal-worker skill instead.
---

# Shoal orchestrator

You run on the user's machine outside any managed workspace and hand work to
agents that each own a workspace. Use `--json` and name every workspace
explicitly: outside a workspace, commands without a target open an interactive
picker. Do an assignment's work in its workspace through its agent, not in your
own checkout. Change or remove another agent's workspace only when the user asks.

Sessions launched through Shoal are scoped to their own workspace and cannot
create workspaces or start sessions elsewhere; you and the user do. Unscoped
commands also decide approval requests, so act on those only as the user directs.

## Start work

```sh
shoal --json repo list
shoal --json add https://github.com/owner/repo/issues/34 --agent happy-codex
shoal --json add my-project fix-login --agent happy-claude
shoal --json add my-project fix-ui --base fix-login --agent happy-claude
shoal --json happy codex fix-login --prompt "Fix the login bug" -- --yolo
```

`add` accepts a registered repository and branch, or an issue, PR or branch URL;
an issue becomes the agent's prompt. Happy agents (`happy-codex`, `happy-claude`)
run detached and appear in the Happy app. CLI agents (`codex`, `claude`) run in
the current terminal until they exit, so use them only when the user asks. A
`--base` naming another workspace's branch stacks the new workspace on it; `ls`
and `status` show the base workspace.

Shoal returns once the launch is recorded (`execution_id`, `pid`, `log`); the
session runs detached and `shoal stop` or `shoal rm` ends it. Issue and `--prompt`
text reaches the agent as its first message (Codex through Happy's server, which
needs this machine's Happy login). Check `prompt_delivered`; when false, the text
is in `prompt_file` and the user must send it from the app.

With `max_agents` configured (the default starts agents one at a time while measured load allows), `add` may queue the agent instead: it then emits
the queue entry (`state: queued`, `position`) and creates nothing yet. The
daemon starts it when a slot frees; `shoal --json ls --queue` lists entries, and
`--now` starts one regardless only when the user asks.

## Follow progress

```sh
shoal --json ls
shoal ls --ready        # PRs, issues and workspaces agents marked ready for review
shoal --json status fix-login
shoal --json notifications
shoal --json internal events --follow --since <id>
```

Notifications report agent exits, messages agents sent with `shoal notify`,
questions they asked, busy resources, pending approval requests and cleanup Shoal did on its own; reading them marks them read. Events stream
workspace lifecycle changes, including `review_ready` marks agents set with
`shoal ready` and `agent_state` turn states (`working`, `waiting`, `idle`) their
hooks report, which `ls` and `status` also show, without consuming notifications; resume from the
last `id` you handled and resync with `shoal --json ls` after a `gap`.

## Approval requests

```sh
shoal --json approvals
shoal approve <number>
shoal deny <number>
```

Report pending requests to the user with their recorded settings and reason,
then approve or deny as the user decides. Resource approval reserves no
capacity; the agent retries its acquisition.

## Questions

```sh
shoal --json questions
shoal answer <number> "<answer or choice number>"
```

Agents ask with `shoal ask`; each question names the decision, the agent's
recommendation and any choices. Relay them to the user and answer as the user
decides; the answer reaches the agent as a message.

## Finish and clean up

Agents run `shoal done` themselves when their assignment is finished. For a
workspace the user wants ended, use `shoal --json done <workspace>`,
`shoal stop <workspace>` to stop its commands, or `shoal rm <workspace>` to
remove it, which asks what to do with unmerged work. To keep one for review,
hold it with `shoal hold acquire <workspace> --name review` and release the hold
afterwards; `shoal restore <workspace>` reopens a removed one, or lists an
existing one's checkpoints, and `shoal restore <workspace> <number>` returns it
to a checkpoint. `shoal --json cleanup` removes every workspace automatic
cleanup would remove without waiting for its idle delay; `--dry-run` lists
them and why it keeps each other workspace, as `status` gives in `cleanup_reason`;
`--caches` also empties shared tool caches nothing is using. When the repository has no remote, `shoal land <workspace>`
merges a finished branch into the default branch; an agent's own `shoal land`
asks for that through `shoal approvals` first.

## Install skills

`shoal skill install` installs this skill and `shoal-worker` into every AI
tool's existing skill directory; a tool name selects one and creates its
directory. Homebrew links follow upgrades; Cargo
installs need refreshing. Run it outside a scoped execution.
