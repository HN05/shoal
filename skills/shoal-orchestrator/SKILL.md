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

Sessions launched through Shoal are scoped to their own space and the spaces
below it, such as child workspaces they split from it with `shoal split`, which
they work on themselves; `shoal start` run there creates children of their space
too, or of a space below it that `--parent` names, and nothing else. A chat is
scoped the same way, to the chat and the spaces below it: the workspaces and
chats it starts go below it; only the user's own terminal is unscoped and
creates top-level spaces. Unscoped commands also
decide approval requests, so act on those only as the user directs.

## Start work

```sh
shoal repo ls
shoal start https://github.com/owner/repo/issues/34 --agent happy-codex
shoal start my-project fix-login --agent happy-claude
shoal start my-project fix-ui --base fix-login --agent happy-claude
shoal start my-project --mode plan --issue 42 --agent claude
shoal happy-codex fix-login -- --yolo
shoal contribute https://github.com/other/repo/issues/12 --agent happy-claude
```

`start` accepts a registered repository and branch, or an issue, PR or branch
URL; an issue becomes the agent's first message, followed by its mode's
instructions. `--mode` picks the mode, `worker` by default, which starts in a
workspace; `chat` and `plan` start in a chat, and `--chat` or `--worktree`
overrides the mode's kind. For a repository the user does not maintain,
`contribute` takes the same URLs: it clones the repository, forks it when the
user cannot push there, and removes the clone once nothing of it is left. Happy agents (`happy-codex`, `happy-claude`)
run detached and appear in the Happy app. Without a terminal, as from your tool
calls, CLI agents (`codex`, `claude`) start detached too, in a zmx session the
daemon opens; `--here` would run one in your tool call until it exits, so pass
it only when the user asks. A
`--base` naming another workspace's branch stacks the new workspace on it; `work`
and `status` show the base workspace. `work` lists child workspaces below their
parent, and `status` names a workspace's parent and children.

Shoal returns once the launch is recorded (`execution_id`, `pid`, `log`); the
session runs detached and `shoal stop` or `shoal rm` ends it. Issue and `--prompt`
text reaches the agent as its first message (Codex through Happy's server, which
needs this machine's Happy login). Check `prompt_delivered`; when false, the text
is in `prompt_file` and the user must send it from the app.

`start` returns once the workspace is set up, emitting it and then the agent's
queue entry; the daemon starts the agent, at once or, with `max_agents`
configured (the default starts agents one at a time while measured load
allows), when a slot frees (`state: queued`, `position`). `shoal queue` lists
entries, and `--now` starts one regardless only when the user asks.
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
shoal approvals --approve <S.N>
shoal approvals --deny <S.N>
```

Approval requests and questions are numbered `S.N`: the number of the space
that asked, then the item's own, such as `3.2`.

Report pending requests to the user with their recorded settings and reason,
then approve or deny as the user decides. Resource approval reserves no
capacity; the agent retries its acquisition.

## Questions

```sh
shoal questions
shoal questions <S.N> --answer "<answer or choice number>"
```

Agents ask with `shoal questions ask`; each question names the decision, the agent's
recommendation and any choices, or, asked in the agent's chat, is its final message
there. Relay them to the user and answer as the user
decides; the answer reaches the agent as a message.

In a chat, you coordinate only the spaces below the chat: the
workspaces you open, which must be in the chat's repository, and those below
them. Their agents report to you instead of the user: their messages,
questions, completion, ready marks and exits arrive as Shoal messages naming
the workspace, the activity of those without an agent of their own arrives with
your Shoal messages, and `shoal files <name>` shows the files a finished
workspace handed back. Their approval requests come to you
too: approve a request to hold a workspace below you with `shoal approvals
--approve <S.N>`, deny one with `--deny <S.N>`, or hand one to the user
with `shoal approvals --escalate <S.N>`, as you must for anything else, since
you hold no grants; `shoal approvals --space <chat>` lists them. Proposed issues
and PRs come too: deny one with `--reason` or hand it to the user, who alone
approves them. You cannot merge, approve or request changes on PRs, approve CI
runs or read the user's notifications; ask the user with `shoal alerts send`.
Open issues with `shoal issue open`; where the repository asks for approval,
it records a proposal for the user, as a workspace's does. Answer what you can
decide with
`shoal questions`, write to an agent with `shoal messages send --space <name>`
(`--attach FILE` hands it a file), and ask the user about the rest. Link the
issues, PRs and CI runs you follow yourself with `shoal link <item> <relation>`:
`shoal view` shows them, and their activity arrives with your Shoal messages.
When the work you follow yourself is ready for the user, run `shoal ready`: it
marks your linked open PRs at their heads, or the chat itself when nothing is
linked, and `shoal ready --undo` withdraws the marks. `shoal ci list`,
`ci rerun`, `ci cancel` and `ci approve` act on the runs of your linked PR, and
`shoal ci run <workflow>` starts one on the default branch or `--branch`, which
your watch then follows. `shoal review run` from the chat reviews your linked
PR in the workspace that owns its branch, and `shoal review list` lists your
linked PRs' reviews.
When the work you were given in the chat is finished, run `shoal done`, with
`--attach FILE` for a report; the user sees the chat as finished until
`shoal done --undo`, and unless cleanup is off, Shoal removes the chat after the
grace period, with the workspaces below it, stopping your agent. A chat stays
after its agent exits until it is removed: the user continues it with
`shoal resume <chat>`.

`shoal start --in <name>` starts a chat in a workspace's worktree, to
debug, audit or review its live work beside the workspace's own agent, which
stays its only editing agent: the chat changes nothing there. In such a chat,
`shoal diff`, `shoal conflicts`, `shoal status` and `shoal branch base` act on
the workspace, while the commands that act on chats act on the chat; pass
`--space <name>` to read the workspace's issue, PRs and CI runs. The chat's
scope reads the workspace and refuses changes to it. Removing the
workspace removes the chat.

To ask the user something in a chat, run `shoal questions ask --in-chat`, then
write the question as your final message and end your turn: it also reaches
the user's questions, so they can answer while away from the chat, and a reply
in the chat closes it. When you must block on the answer, put the decision on
the command line with `shoal questions ask "<decision>" --recommend <answer>
--wait`. Either way an answer from their questions arrives as a Shoal message.

## Finish and clean up

Agents run `shoal done` themselves when their assignment is finished. For a
workspace the user wants ended, use `shoal done <workspace>`, or
`shoal done --integrated <workspace>` once another branch took its commits under
new SHAs,
`shoal stop <workspace>` to stop its commands, or `shoal rm <workspace>` to
remove it, which asks what to do with unmerged work. To keep one for review,
hold it with `shoal acquire hold --workspace <workspace> --name review` and release the hold
afterwards. Removing a workspace removes the spaces below it first,
held ones included, and its `done` with cleanup removes the unheld ones after
the grace period. `shoal work restore <workspace>` reopens a removed one, or lists an
existing one's checkpoints, and `shoal work restore <workspace> <number>` returns it
to a checkpoint. To try another approach from a workspace's current state, debug
it without touching it, or give its state to a second agent, start a copy:
`shoal start --copy <workspace>`, with `--fork` to fork its agent's session, or
`--checkpoint <number>` for one of its checkpoints, of a live or removed
workspace; `shoal split --copy <workspace>` creates one without an agent. To move a workspace to another machine, run
`shoal move ssh <host> <workspace> --yes`, which queues its stopped agents there;
they continue their sessions as that host's agent limits allow. Over another
medium, pipe `shoal move export <workspace> --yes` into `shoal move import --resume`
on the other machine. `shoal cleanup spaces` removes every space automatic
cleanup would remove without waiting for its idle delay; `shoal cleanup --dry-run`
lists those, kept branches and shared caches, and why it keeps each other space,
as `status` gives in `cleanup_reason`; `shoal cleanup caches` empties shared tool
caches nothing is using. When the repository merges locally instead of through PRs, `shoal merge <workspace>`
merges a finished branch into the default branch, and `shoal unmerge <workspace>`
undoes its latest merge while the default branch is unpushed; an agent's own
`shoal merge` or `shoal unmerge` asks for that through `shoal approvals` first. A merge that pushes asks for
confirmation, which needs a terminal; pass `--yes` only when the user asked for
the push.

## Install skills

`shoal skill install` installs this skill and `shoal-worker` into every AI
tool's existing skill directory; a tool name selects one and creates its
directory. Homebrew links follow upgrades; Cargo
installs need refreshing. Run it outside a scoped execution.
