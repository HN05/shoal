---
name: shoal-orchestrator
description: Use outside any Shoal workspace to coordinate work for the user — create workspaces for issues or branches, start detached agent sessions, follow their progress, answer approval requests, and clean up. Agents working inside a Shoal workspace use the shoal-worker skill instead.
---

# Shoal orchestrator

You run on the user's machine outside any managed workspace and hand work to
agents that each own a workspace. Name every workspace explicitly: outside a
workspace, commands without a target open an interactive picker.

Commands print JSON and never prompt when `SHOAL_OUTPUT=json`, which Shoal sets
for the chats it starts. Without it, as in a session Shoal did not start or a
command run over `ssh`, pass `--json` to each command. A command that prints
text despite the setting comes from a Shoal that predates it; pass `--json` too. Do an assignment's work in its workspace through its agent, not in your
own checkout. Change or remove another agent's workspace only when the user asks.

Sessions launched through Shoal are scoped to their own workspace and the
child workspaces they split from it with `shoal split`, which they work on
themselves; they cannot create other workspaces or start sessions elsewhere;
you and the user do. Unscoped
commands also decide approval requests, so act on those only as the user directs.

## Start work

```sh
shoal repo ls
shoal add https://github.com/owner/repo/issues/34 --agent happy-codex
shoal add my-project fix-login --agent happy-claude
shoal add my-project fix-ui --base fix-login --agent happy-claude
shoal happy codex fix-login --prompt "Fix the login bug" -- --yolo
shoal contribute https://github.com/other/repo/issues/12 --agent happy-claude
```

`add` accepts a registered repository and branch, or an issue, PR or branch URL;
an issue becomes the agent's prompt. For a repository the user does not maintain,
`contribute` takes the same URLs: it clones the repository, forks it when the
user cannot push there, and removes the clone once nothing of it is left. Happy agents (`happy-codex`, `happy-claude`)
run detached and appear in the Happy app. CLI agents (`codex`, `claude`) run in
the current terminal until they exit, so use them only when the user asks. A
`--base` naming another workspace's branch stacks the new workspace on it; `work`
and `status` show the base workspace. `work` lists child workspaces below their
parent, and `status` names a workspace's parent and children.

Shoal returns once the launch is recorded (`execution_id`, `pid`, `log`); the
session runs detached and `shoal stop` or `shoal rm` ends it. Issue and `--prompt`
text reaches the agent as its first message (Codex through Happy's server, which
needs this machine's Happy login). Check `prompt_delivered`; when false, the text
is in `prompt_file` and the user must send it from the app.

With `max_agents` configured (the default starts agents one at a time while measured load allows), `add` may queue the agent instead: it then emits
the queue entry (`state: queued`, `position`) and creates nothing yet. The
daemon starts it when a slot frees; `shoal queue` lists entries, and
`--now` or `--focus` starts one regardless only when the user asks.
`--after <workspace|queued agent|issue>` (repeatable) queues an agent until that
work is done; an issue's agent also waits for the issues its forge records as
blocking it. The agent then starts on the done workspace's branch, stacked on
it, and the entry lists what it still waits for in `waiting_for`.

## Follow progress

```sh
shoal work
shoal inbox      # Questions, approvals, waiting or failed workspaces, ready marks
shoal status fix-login
shoal alerts
shoal internal events --follow --since <id>
```

Notifications report agent exits, messages agents sent with `shoal alerts send`,
questions they asked, busy resources, pending approval requests and cleanup Shoal did on its own; reading them marks them read. Events stream
workspace lifecycle changes, including `review_ready` marks agents set with
`shoal ready` and `agent_state` turn states (`working`, `waiting`, `idle`) their
hooks report, which `work` and `status` also show, without consuming notifications; resume from the
last `id` you handled and resync with `shoal work` after a `gap`.

## Reviews

```sh
shoal review run fix-login     # Queue a read-only review of its committed HEAD
shoal review run <pr-url>      # Review a PR in the workspace that owns its branch
shoal review list fix-login    # Its reviews, newest first, with unresolved findings
shoal view review 17           # One review in full
shoal watch review 17          # Wait until it ends
shoal review cancel 17
```

A finished review reaches the workspace's agent as a message with its findings,
and you, coordinating, as one line. The agent resolves each finding; relay a
finding it asks about to the user.

## Approval requests

```sh
shoal approvals
shoal approvals --approve <number>
shoal approvals --deny <number>
```

Report pending requests to the user with their recorded settings and reason,
then approve or deny as the user decides. Resource approval reserves no
capacity; the agent retries its acquisition.

## Questions

```sh
shoal questions
shoal questions <number> --answer "<answer or choice number>"
```

Agents ask with `shoal questions ask`; each question names the decision, the agent's
recommendation and any choices, or, asked in the agent's chat, is its final message
there. Relay them to the user and answer as the user
decides; the answer reaches the agent as a message.

In a `shoal chat`, the agents of the workspaces you open report to you instead
of the user: their messages, questions, completion, ready marks and exits
arrive as Shoal messages naming the workspace, and `shoal files <name>` shows the
files a finished workspace handed back. Answer what you can decide with
`shoal questions`, write to an agent with `shoal messages send --workspace <name>`
(`--attach FILE` hands it a file), and ask the user about the rest.

## Finish and clean up

Agents run `shoal done` themselves when their assignment is finished. For a
workspace the user wants ended, use `shoal done <workspace>`, or
`shoal done --integrated <workspace>` once another branch took its commits under
new SHAs,
`shoal stop <workspace>` to stop its commands, or `shoal rm <workspace>` to
remove it, which asks what to do with unmerged work. To keep one for review,
hold it with `shoal acquire hold --workspace <workspace> --name review` and release the hold
afterwards. Removing a workspace removes the child workspaces below it first,
held ones included, and its `done` with cleanup removes the unheld ones after
the grace period. `shoal work restore <workspace>` reopens a removed one, or lists an
existing one's checkpoints, and `shoal work restore <workspace> <number>` returns it
to a checkpoint. To move a workspace to another machine, run
`shoal move ssh <host> <workspace> --yes`, which queues its stopped agents there;
they continue their sessions as that host's agent limits allow. Over another
medium, pipe `shoal move export <workspace> --yes` into `shoal move import --resume`
on the other machine. `shoal cleanup workspaces` removes every workspace automatic
cleanup would remove without waiting for its idle delay; `shoal cleanup --dry-run`
lists those, kept branches and shared caches, and why it keeps each other workspace,
as `status` gives in `cleanup_reason`; `shoal cleanup caches` empties shared tool
caches nothing is using. When the repository has no remote, `shoal land <workspace>`
merges a finished branch into the default branch; an agent's own `shoal land`
asks for that through `shoal approvals` first.

## Install skills

`shoal skill install` installs this skill and `shoal-worker` into every AI
tool's existing skill directory; a tool name selects one and creates its
directory. Homebrew links follow upgrades; Cargo
installs need refreshing. Run it outside a scoped execution.
