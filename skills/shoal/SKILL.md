---
name: shoal
description: What Shoal's commands do, for agents in a Shoal workspace or chat and for sessions outside one — start and coordinate agents, open, change, watch and review issues and PRs, mark work ready, ask the user, hand back files, finish with `shoal done`, sync the default branch, split child workspaces, and reserve ports, simulators, resource permits and secrets.
---

# Shoal

Commands print JSON and never prompt when `SHOAL_OUTPUT=json`, which Shoal sets
for the agents it launches; otherwise `--json` on each command does the same.

## Spaces and scope

A space is a workspace, with its own worktree and branch, or a chat, with none.
Spaces form a tree: each has at most one parent. Agents Shoal launches are
scoped to their own space and the spaces below it; only the user's own
terminal is unscoped. A master session, a chat without a repository that
`shoal start --mode orchestrate` starts in `root_dir`, has a scope that reaches
every space. Inside a space, commands without a target act on it, or
on a child workspace when run inside the child's directory; outside one, they
open a picker on a terminal. A session Shoal did not launch in a managed
worktree resolves the workspace from its directory, without scope or lifecycle
tracking; `shoal work` lists the workspaces with their `path`. Leases need a
managed workspace.

Every space has a number, which `shoal work` and `shoal status` show. A
command that takes a space selects it by that number, its name or its ID:
`shoal attach 3`, `--space 3`. Space names are never all digits, so
`shoal work rename` refuses one. A removed workspace has no number. Approval
requests and questions are numbered `S.N`: the number of the space that asked,
then the item's own, such as `3.2`.

A chat in a workspace's worktree (`shoal start --in <workspace>`) reads that
workspace and changes nothing there: `shoal diff`, `shoal conflicts`,
`shoal status` and `shoal branch base` act on the workspace, the commands that
also act on chats act on the chat, and `--space <workspace>` reads the
workspace's issue, PRs and CI runs. Removing the workspace removes the chat.

## Start agents

```sh
shoal repo ls
shoal start https://github.com/owner/repo/issues/34 --agent claude
shoal start my-project fix-login --agent codex
shoal start my-project fix-ui --base fix-login --agent claude
shoal start my-project --mode plan --issue 42 --agent claude
shoal start --issue 43 --agent claude
shoal start my-project api-part --agent codex -- "Move the API client into its own crate"
shoal claude fix-login
shoal contribute https://github.com/other/repo/issues/12 --agent claude
```

`start` accepts a registered repository and branch, or an issue, PR or branch
URL; an issue becomes the agent's first message, followed by its mode's
instructions, and text after `--` is the task. `--mode` picks the mode,
`worker` by default, which starts in a workspace; `chat` and `plan` start in a
chat, and `--chat` or `--worktree` overrides the mode's kind. `--base` naming
another workspace's branch stacks the new workspace on it. `contribute` takes
the same URLs for a repository the user does not maintain: it clones it, forks
it when the user cannot push there, and removes the clone once nothing of it is
left.

Run inside a space, `start` creates a child of it, or of a space below it that
`--parent` names, in its repository, or for a master session in any registered
one, on a branch from the default branch;
`--chat` starts a child chat in a background place and returns once the chat
has started, and `--here` is refused. Outside any space it creates top-level
spaces. Without a terminal, as from a tool call, agents start detached; `--here`
runs one in the calling terminal until it exits. `--fork` starts the caller's
own agent from a fork of its session, so it needs only its task after `--`.

`start` returns once the workspace is set up, with the workspace and the
agent's queue entry. The daemon starts queued agents as `max_agents` allows
(`state: queued`, `position`); `shoal queue` lists entries and `--now` starts
one regardless. `--after <workspace|queued agent|issue>` (repeatable) holds an
agent until that work is done, where a bare number is an issue and a workspace
is named by name or ID, and an issue's agent also waits for the issues
its forge records as blocking it; the agent then starts on the done workspace's
branch, and `waiting_for` lists what it still waits for. A Happy agent reports
its launch (`execution_id`, `pid`, `log`); `prompt_delivered: false` means the
first message is in `prompt_file` for the user to send from the app.

## Child workspaces

```sh
shoal split fix-typo                  # New branch from the default branch
shoal split --issue 42                # Named after the issue, which it links
shoal split api-part --base <branch>  # Stacked on the given branch
shoal split --no-branch [--base <ref>]  # A workspace at a commit, no branch
shoal split --copy                    # A copy of this workspace's files
```

`split` creates a child of the current workspace with its own worktree, branch
and setup, without an agent, and returns its `path`. Shoal commands run in the
child's directory act on the child. The child's issue and PR activity reaches
the parent's agent, each update naming the child. `--no-branch` has no branch,
PR or agent of its own. `--copy` gives the child the committed, uncommitted,
untracked and ignored files, and `--checkpoint <number>` one of the
checkpoints' files instead; an untouched copy goes with cleanup. `shoal start
--copy [--fork] -- "<task>"` starts an agent in such a copy, and
`shoal start --copy <workspace>` copies another workspace, live or removed.
`shoal rm <child>` removes a child, including one a split left `preparing`;
one the user holds, or with such a space below it, is the user's to remove.

## Coordinate spaces below

The agent of the nearest space above with an agent coordinates a space: its
agent's messages, questions, completion, ready marks and exits arrive there as
messages naming it, instead of reaching the user, and the issue and PR
activity of a space without an agent of its own arrives too. An agent started
with `shoal start` receives its own issue and PR activity.

```sh
shoal messages send --space <child> "<text>"   # --body-file F, --attach FILE
shoal questions <S.N> --answer "<answer or choice number>"
shoal stop <child>                  # --skip-children stops only the child
shoal restart <child>               # Its agent, a workspace's or a chat's
shoal resume <child>                # --discard forgets the saved session
shoal files <child>
```

`stop` stops the child's agent and those below it, keeping their sessions, and
cancels agents queued to start there; `restart` stops the child's agent and
starts its session again at once; `resume` queues the agents the caller
stopped and restores the chats its restart left stopped in a background
place, while the user resumes the ones the user stopped.

A child's approval request comes to its coordinator first.
`shoal approvals --approve <S.N>` works only for a request to hold a workspace
below, or for a thing the coordinator holds an approved grant for; otherwise
it is refused. `--deny <S.N>` denies it, and `--escalate <S.N>` passes it to the
space above or the user. Only the user approves a proposed issue or PR; a
coordinator may deny one with `--reason` or pass it up.

## Follow progress

```sh
shoal work            # Spaces, children below their parent; --chats for chats
shoal inbox           # Questions, approvals, waiting or failed spaces, ready marks
shoal status fix-login
shoal alerts
```

`status` names a workspace's base, parent and children, its watched PRs in
`prs` with state, merge conflicts, CI results, review state and lookup errors,
and in `cleanup_reason` why cleanup keeps it. `work` and `status` show the
`agent_state` agents' hooks report (`working`, `waiting`, `idle`).
Notifications report agent exits, alerts, questions, busy resources, pending
approvals and cleanup Shoal did on its own; reading them marks them read, and
scoped callers cannot read them.

## Space title

`shoal work rename "<title>"` names the space's work; the tabs and windows
Shoal opens for it show the title instead of its branch or issue number.

## Issues and pull requests

```sh
shoal pr open [--title T] [--body-file F] [--label L] [--draft]
shoal pr edit --title T --add-label L --no-draft
shoal pr comment --body-file F [--attach FILE]
shoal pr reply <comment-id> --body-file F
shoal pr resolve <comment-id> --body-file F
shoal pr react +1 [--comment <comment-id>] [--remove]
shoal pr review [<number>] --comment --body-file F --line PATH:LINE 'text'
shoal issue open --title T --body-file F [--link] [--repo OWNER/NAME]
shoal issue list <words> [--state all] [--label L] [--author A] [--limit N]
shoal repo labels
```

`pr open` pushes the branch, then links the branch's open PR or opens one. A
linked issue supplies the default title and, without a body, `Closes #<number>`;
a supplied body is used verbatim. Opening refuses an item without a label
`required_labels` asks for; `pr open`, `issue open` and `ready` report
`warnings` for missing labels or template headings. `repo labels` lists each
label with its meaning. Shoal acts as the agent account and records each
change it makes. `pr edit` also takes `--body-file`, `--add-reviewer`
(`shoal pr reviewers` lists who can review), `--add-assignee`, `--milestone`
and `--draft`; `pr comment --edit <comment-id>` changes a comment the agent
wrote; `pr close` closes; `issue` has the same commands. `open`, `edit` and
`comment` take `--attach FILE`, and the body shows images and videos and links
other files where it references them as `![alt](FILE)`. `shoal view` shows
each review comment's ID; `reply` answers in its thread, and `resolve` replies
and then resolves the thread. `view --json` lists reactions. `pr review` adds
comments on lines of the diff: `--line`, repeated, `--old-line` for a removed
line, or `--comments-file F` with a JSON array of `{"path", "line", "body"}`
objects, with `"side": "old"` for a removed line. A comment on a line the diff
does not show refuses the whole review before anything is posted. These
commands act on the linked item, or on the number or URL given.

`issue open` records the issue as opened by the space, and only `--link`
assigns it. `--repo OWNER/NAME`, or a repository URL on another forge, opens it
in another repository, as `[permissions] issue_other_repositories` allows;
`issue list` and `pr list` take `--repo` too. `shoal pr revert [<number>]`
opens a PR that undoes a merged one; where the forge cannot, it opens it from a
child workspace already marked done, which goes once the revert merges.
Workspace processes cannot merge a PR, delete its branch, approve or request
changes on one, or approve CI runs: `pr merge`, `pr delete-branch`,
`pr review --approve`, `--request-changes` and `ci approve` are refused.

`shoal forge capabilities` lists what the forge supports, such as resolving
threads, rerunning jobs or reading job logs, and what to do instead.
`shoal forge api 'repos/{owner}/{repo}/<path>'` sends a GET to the forge's API
and prints the JSON, `--repo OWNER/NAME` for another repository; other methods
are refused unless the repository sets `[permissions] api_writes`.
`shoal forge download <url> --output <file>` saves an image or attached file
from the forge with the same login.

### Proposals and pushes

Where a repository's `[permissions]` require the user's approval for issues
and PRs, `issue open` and `pr open` exit 0 with `proposed`, an approval
request, instead of the item; `pr open` still pushes unless pushes need
approval too. Only the user approves a proposal, and may edit its title and
description first; the decision arrives as a message naming the opened item,
or giving a reason or the duplicate issue for a denial. A proposed issue stays
after the space is removed. A pending PR proposal keeps the workspace; a new
commit outdates it until `pr open` runs again. A chat proposes issues, never
PRs.

With `[permissions] push` requiring approval, `pr open` pushes nothing: it
proposes the PR together with the push, or only the push for an open PR. Plain
`git push` reports the push as rejected and `waits for approval <S.N>`;
`--force-with-lease` works as usual. On approval Shoal pushes the
approved commit and opens the PR that waited for it. A new commit outdates the
request until the next push or `pr open`. A pending push keeps the workspace.
With `push = "deny"`, `pr open` and `git push` fail with the reason.

## Links and watches

```sh
shoal link pr <number-or-url> assigned
shoal link ci <run-url> related
shoal unlink pr [<number-or-url>] assigned
shoal view [pr <number> | <url>] [--no-comments] [--check '<name>']
shoal watch pr [--timeout <seconds>]
```

A link names its relation last: `assigned` for the space's own work, `opened`
for an item it created another way, `related` for one the work relates to.
Assigned PRs accumulate per workspace; after `done`, cleanup waits until all
have merged and contain HEAD, or the default branch has their changes, and a
closed, unmerged PR keeps it waiting until it is unlinked. `unlink` without a
number unlinks every assigned PR.

`shoal view` shows the linked items with the linked issue, each item's
description, comments and reviews with inline comments, and the log tails and
URLs of failed CI jobs where the forge reads job logs; `--check` selects one
check's log.

Shoal reports the activity of watched items: comments and reviews; edits to an
item's title, description, labels, assignees, milestone, draft state or base
branch; a failed CI check with its failing step when the runner records it;
the passed checks once none on the PR's head is pending; a merge conflict,
again after a push or base change that leaves it conflicting; closing and
merging. It leaves out what the space itself did through Shoal. For Claude
Code, and for a workspace's Codex in a zmx session, Rex block or Herdr pane,
launched through Shoal, hooks deliver activity as context after tool calls and start the next
turn with it after a turn ends; a delivery replaces a running watch. Other
agents run `shoal watch pr`, which returns `updates`. The first wait includes
existing activity, and later waits share a cursor per workspace. A newer wait
supersedes a running one, which returns `superseded: true`. Without
`--timeout` (1–3600 seconds) the wait is unbounded; `timed_out` with empty
`updates` means no update. A `lookup_failed` entry names a failed lookup and
repeats every 10 minutes while it persists. A `base_merged` update means the
base workspace merged; it says whether Shoal retargeted the PR and gives the
rebase command.

## CI

```sh
shoal ci list [--pr N]
shoal view ci <run-url>
shoal ci rerun [--check '<name>'] [--pr N]
shoal ci rerun <run-url> [--failed]
shoal ci cancel [--check '<name>' | <run-url>]
shoal ci run <workflow> [--input NAME=VALUE] [--branch B]
```

`ci rerun --check` reruns a failed check on the linked PR's current head, and
without `--check` every failed one; `ci cancel` cancels running ones. With a
run URL both act on one run of the space's branch. `ci run` starts a workflow,
by its file name or its link, on the space's branch, or for a chat on the
default branch or `--branch`. It is a new run, not a rerun: on Forgejo a failed
check on the PR stays failed. When `ci run` links the run as opened, a watch
reports when it finishes. Runs of other repositories are the user's to read,
link and act on. A PR from a fork or first-time contributor can show runs that
wait for approval instead of checks; `shoal view` lists them in `waiting_runs`.

## Local reviews

```sh
shoal review run                    # Or a workspace name, or a PR URL
shoal review list                   # Reviews, newest first, with unresolved findings
shoal view review [<id>]
shoal watch review [<id>]
shoal review resolve 17.1 fixed
shoal review resolve 17.2 false-positive --reason "The caller checks the length"
shoal review resolve 17.3 wont-fix --reason "..."
shoal review resolve 17.4 deferred --issue 1240
shoal review resolve 17.5 duplicate --of 17.2
shoal review reply 17.6 "Only on empty input?"
shoal review cancel [<id>]
```

`review run` queues a one-shot, read-only review of the committed HEAD;
uncommitted edits stay out of it. With a PR URL, or from a chat for its linked
PR, it reviews the PR in the workspace that owns its branch. The findings arrive at the
workspace's agent as a message, each numbered `<review>.<n>`, and at its
coordinator as one line. `resolve` records an outcome with its data, and
`reply` discusses a finding without resolving it. Later reviews of the
repository receive each `false-positive` finding's title, path and reason and
skip that claim while the reason holds; the next review checks each `fixed`
finding. When the repository requires a review, `pr open` of a new PR and
`ready` are refused until a finished review covers HEAD with every finding
resolved, and the refusal starts one when none exists.

## Ready marks

`shoal ready` marks the linked items, or the space when nothing is linked,
ready for review at HEAD for the user's integrations, and takes a linked draft
PR out of draft unless `[pr] undraft_on_ready = false`. It skips a linked PR
that is not open or whose head on the forge is not HEAD, naming each with its
reason on stderr. `shoal ready --undo` withdraws the marks, and `--draft` also
returns the PR to draft.

## Messages, alerts and questions

The user, Shoal and a coordinator send messages to a space, such as a request
to free memory or disk space. Hooks deliver them with PR activity; a new
message also ends `shoal watch pr` with a `messages` array, and
`shoal messages` shows new ones. Each message is shown once, and lists each
file attached to it as an `Attached file:` path.

`shoal alerts send "<one-line message>" [--attach FILE]` tells the user
something without asking, and `shoal alerts withdraw <number>` withdraws it
before the user reads it. Neither affects completion or cleanup.

```sh
shoal questions ask --in-chat
shoal questions ask "Which database?" --recommend SQLite --reason "No server to run" \
  --choice SQLite --choice Postgres [--attach FILE] [--wait]
shoal questions                     # Open questions of the space
shoal questions withdraw <S.N>
```

A question reaches the space's coordinator, or else the user's questions.
`--in-chat` makes the turn's final message the question and puts it among the
questions too; a reply in the chat closes it. It takes no attachments,
and is refused for an agent without Shoal's hooks. The other form states the
decision, a recommended answer and optional choices. An answer given through
`shoal questions` arrives as a message naming the question. `--wait` blocks without a
time limit and prints the answer. Asking the identical question again waits
for the same one. An open question keeps the space from cleanup, and
`shoal done` closes the space's open questions.

## Files

```sh
shoal files [space]
shoal files add notes.md ['chart.html#Latency by region'] [--space S]
shoal files rm [FILE...] [--space S]
shoal done --attach report.md
```

`files add` copies files of at most 100 MiB into the space's attached files
without telling its agents; `PATH#TITLE` gives a title. `files` lists them, and
`rm` deletes copies. Files handed back with `done --attach`, and those sent with
alerts and questions, outlive the space for the retention period; the
coordinator, or else the user, reads them with `shoal files <space>`.

## Completion

```sh
shoal done [space] [--attach FILE]... [--keep-demos]
shoal done --integrated <workspace>
shoal done --undo [space]
```

`shoal done` records that the space's work is finished at its HEAD, notifies
its coordinator or else the user, closes its open questions and stops its
demos unless `--keep-demos`. Only `done` records completion; issue closure and
merged PRs do not. With cleanup, the default unless `[cleanup.done] enabled =
false` (`--cleanup` overrides it), Shoal removes the space and the spaces below
it after the grace period (`[cleanup] grace_minutes`), stopping their agents:
a workspace once every assigned PR has merged and contains HEAD, or without a
PR once its files are clean and every commit is pushed, on the default branch,
or on the branch it builds on; a chat whatever PRs it watches. Holds, open
questions and pending proposals or pushes keep a space. Each removed space but
a disposable one keeps a checkpoint and its branch, and `shoal work restore`
reopens it. `completion.error` and `pr_cleanup.error` report cleanup or lookup
failures.

`--integrated <workspace>` records that another branch has that workspace's
work under new commits: until its HEAD moves, its unpushed commits no longer
keep it, and its conflict watch stops. `--undo` withdraws a recorded
completion; associations and watches stay, closed questions stay closed, and
cleanup proceeds as if `done` had not run.

A merged workspace nobody has used for hours goes with merged cleanup whether
or not `done` ran.

```sh
shoal acquire hold --name <name> --reason <why> [--workspace <child>]
shoal leases hold
shoal release hold --name <name>
```

A hold keeps a workspace from cleanup. A workspace process asking to hold its
own workspace makes an approval request to its coordinator, or the user;
holding a workspace below it needs none. Scoped callers release only the holds
agents placed.

## Remove, restore and move

```sh
shoal rm <space>
shoal work checkpoint --name "<what comes next>"
shoal work restore <workspace> [<number>]
shoal cleanup spaces
shoal cleanup --dry-run
shoal cleanup caches
shoal move ssh <host> <workspace> --yes
shoal move export <workspace> --yes | shoal move import --resume
```

Removing a space removes the spaces below it first, held ones included, and
asks what to do with unmerged work. A checkpoint keeps HEAD and unsaved files;
Shoal also takes one when an agent exits or is stopped. `work restore` reopens
a removed workspace, or lists an existing one's checkpoints, and with a number
returns to one after keeping the current state in a new checkpoint; it refuses
while another command runs in the workspace. `cleanup spaces` removes every
space automatic cleanup would remove without waiting for its idle delay;
`--dry-run` lists those, kept branches and shared caches, and why each other
space stays. `move ssh` moves a workspace to another machine and queues its
stopped agents there; with `--keep`, it copies the workspace there as a new one
and leaves it running here.

## Branches

```sh
shoal repo sync
shoal conflicts [<branch>]
shoal branch base [set <workspace>]
shoal merge [<workspace>]
shoal unmerge [<workspace>]
```

The daemon keeps the local default branch in sync with its remote, within
about a minute. `repo sync` syncs it at once: it fetches the remote and
fast-forwards the local default branch, advancing the registered checkout when
it has that branch checked out, where `git fetch origin main:main` is refused.
It never pushes or changes a workspace's branch. Branches of other workspaces
are shared refs, and pushed ones are current as `origin/<branch>` after a sync.
`conflicts` checks committed HEAD against the base workspace's branch,
otherwise the local default branch, or the branch named, without touching the
worktree; exit 1 lists the conflicting paths. It also lists a file the
repository marks as holding a number each change picks, such as a migration,
under "Changed on both sides" when both branches changed it. Without a watched
PR, the watch reports a `merge_conflict` when the check newly fails.
`branch base set` records that a workspace builds on another's branch.

Where the repository merges locally instead of through PRs, `merge` merges a
committed branch into the default branch and `unmerge` undoes its latest merge
while the default branch is unpushed. A workspace process's `merge` or
`unmerge` first exits 2 with `approval_pending` for its current HEAD, unless
`[permissions] merge = "direct"`; after approval the same command at the same
HEAD proceeds, and a new commit needs a new request. A merge that pushes asks
for confirmation on a terminal, and `--yes` gives it.

## Approvals

```sh
shoal approvals
shoal approvals --approve <S.N>
shoal approvals --deny <S.N>
shoal approvals withdraw <S.N>
```

A command that needs approval returns `approval_pending` with the request
number, and `approval_denied` once denied; rerunning it after approval
proceeds. `--reason` states the purpose of protected access. A request goes to
the nearest space above with an agent, then up to the user. Unscoped callers
decide any request. Approving a resource reserves no capacity.

## Ports

```sh
shoal catalog port
shoal leases port
shoal acquire port web [--port N] [--reason "purpose"]
shoal release port web
```

`catalog port` lists the configured names and defaults. Repeating a
reservation returns the same port. Exit 2 with `reserved: false` is a conflict
suggestion, not a reservation; repeating the request with
`--port <suggested_port>` reserves it. The returned `port` is the reserved one.
New reservations do not change the environment of running commands.
Reservations and leases outlive the command that made them.

## Resource pools

```sh
shoal catalog resource
shoal leases resource
shoal acquire resource devices [--member M] [--name N] [--mode read|write] [--queue]
shoal release resource devices [--name N]
```

The result names the leased `resource`. Each semaphore lease takes one permit;
the same `--name` returns the same lease, and release without `--name` frees
every lease in the pool. Busy requests exit 2. `--queue` waits, without a time
limit, until the lease is granted or its approval denied, taking capacity in
arrival order; stopping it gives up the place, and while others are queued,
requests without it are busy. An `rwlock` resource has shared readers and
exclusive writers (`--mode`, write by default); `read_available` and
`write_available` give its capacity, and changing mode needs a release first.
Use is cooperative.

## Simulators

```sh
shoal catalog sim
shoal leases sim
shoal acquire sim [--queue]
shoal acquire sim --clean --reason "Verify first-launch permission prompts"
shoal release sim
```

`catalog sim` lists the configured profiles and installed runtimes.
Acquisition returns an exclusive lease on a device with its `udid`; simctl's
`booted` selector can name another workspace's device. Shoal creates, boots,
shuts down, erases and deletes its devices, choosing one that minimizes
reinstalls, and normal reuse keeps apps, data and settings. Exit 2 means
capacity is busy. `--clean` erases the device, refuses while a lease is held,
and records the reason and outcome in an audit trail. Only installed runtimes
are supported.

## Secrets

`shoal catalog secret` lists the configured secrets and which need approval. A
tool whose secret needs approval fails with the request to make:
`shoal acquire secret NAME --reason "purpose"`. The value never reaches the
agent's environment; `shoal release secret NAME` gives up the grant.

## Demos

```sh
shoal demo ls                      # Configured and running demos
shoal demo start                   # The repository's only configured demo
shoal demo start web               # A configured demo by name
shoal demo start tui -- ./target/debug/app   # Any command, named tui
shoal demo start site --preset web -- npm run dev
shoal demo screen tui              # What the demo shows now
shoal demo send tui $'q'           # Type into it; $'\r' is Enter
shoal demo stop tui
```

Shoal keeps a demo's command running, restarts it when it exits, and tells the
user how to open it; `shoal demo` and the inbox open it for them. The `server`
preset reserves a port and passes it as `PORT`, and `web` does the same for a
page opened in a browser; the `simulator` preset leases the workspace's
simulator and passes its UDID as `SHOAL_SIM_UDID`. Chats have no demos.

## Processes

After a tool call Shoal names processes the space's commands left running
after their parent exited, such as a failed test run's servers.
`shoal stop --leftovers` stops them, and `shoal stop --processes` stops every
process of the workspace Shoal does not track, never the agent or its shells.
`shoal work setup` reruns the configured setup command and post-setup hook.

## Daemon

The daemon, its service and the Shoal installation belong to the user.
Stopping the daemon stops every agent, and while it restarts every agent's
`shoal` commands fail. `shoal daemon reload` applies the config on disk without
stopping anything; an invalid file leaves the previous settings in place.

## Skill

`shoal skill` prints this skill. `shoal skill install` installs it into every
AI tool's existing skill directory, and a tool name selects one and creates
its directory; it is refused in a scoped execution. Later unscoped commands
keep installed copies current.
