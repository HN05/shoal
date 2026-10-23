---
name: shoal-worker
description: Use inside a Shoal workspace to finish assignments with `shoal done`, mark work ready for review, notify the user, ask the user questions, open, change, watch and wait on PRs and issues, sync the default branch from its remote, reserve ports, lease Xcode simulators, and acquire resource permits during development or testing. Applies to agents launched through Shoal or working directly in a Shoal worktree.
---

# Shoal worker

Use `--json`, omit targets for the current context, and request only needed resources.
When acquisition returns `approval_pending`, give the user the request number and wait
for an unscoped `shoal approve <number>` or `shoal deny <number>`. When you no
longer need a pending request, withdraw it with
`shoal --json approvals withdraw <number>`. Supply `--reason` for
protected access; inspect your requests with `shoal approvals`. Retry after approval,
and stop on `approval_denied`. Never remove scope to approve your own request.

## Completion

Shoal removes your workspace after you run `shoal done`, or once its branch has
merged and nothing has used it for hours; issue closure and merged PRs do not
end the assignment. Run `shoal done` as your last command once the
assignment is finished: every watched PR merged, or no PR needed, in your
workspace and in the child workspaces you split from it. Never end a
session without it.

Open your PR with `shoal --json pr open`: it pushes your branch, then links the
branch's open PR or opens one (`--title`, `--body-file`, `--label`, `--draft`).
A linked issue supplies the default title and, when no body is supplied,
`Closes #<number>`. A supplied body is used verbatim; include a closing reference
yourself when the PR completes the issue. Before opening an issue or PR, read the
repository's contribution rules and forge template for what the description
needs, and choose labels from `shoal labels`, which lists each with its meaning.
Opening refuses an item without a label `required_labels` asks for; `pr open`,
`issue open` and `ready` report `warnings` for missing labels or template
headings, which you fix with `edit`. Change your PR or issue through Shoal,
which acts as your agent account and records the change: `shoal --json pr edit`
(`--title`, `--body-file`, `--add-label`, `--add-reviewer`,
`--draft`/`--no-draft`), `pr comment --body-file`, `pr close`, and the same
`issue` commands. `open`, `edit` and `comment` take `--attach FILE` for
screenshots and videos, which the body shows where it references them as
`![alt](FILE)`. Answer each review
finding in its thread: `shoal view` shows every review comment's ID, and
`shoal pr reply <comment-id> --body-file F` replies there, while
`shoal pr resolve <comment-id> --body-file F` replies and then resolves the thread.
They default to the linked item; pass a number or URL for another item in the
repository. `shoal --json capabilities` lists what the repository's forge
supports, such as resolving threads, rerunning jobs or reading job logs, with
what to do instead; check it before relying on one. When no command covers a
read, `shoal --json api 'repos/{owner}/{repo}/<path>'` sends a GET to the
repository's forge API and prints the JSON; use it instead of `gh api` or
`glab api`. It refuses other methods unless the repository sets `[api]
agent_writes`. File
follow-up work with `shoal issue open --title`; Shoal records it as opened by your
workspace, and only `--link` assigns it to you. A problem that belongs to another
repository, such as a tool the project uses, goes there with `--repo OWNER/NAME`
(or a repository URL on another forge); it usually becomes a proposal, and the
repository may refuse it. Before opening one, search for an
existing issue with `shoal --json issue list <words>` (`--state all`, `--label`,
`--author`, `--limit`; `pr list` takes the same), and comment there instead of
opening a duplicate. Merging is
the user's: `shoal pr merge` refuses workspace processes.

A repository can require the user's approval for issues and PRs you open
(`[approvals]`). Then `issue open` and `pr open` exit 0 with `proposed`, an
approval request, instead of the item; `pr open` still pushes. Only the user decides
proposals, never a coordinating agent. Continue your work and do not open the
item another way. The user may edit the title and
description, and the decision arrives as a message: approval names the opened
item, which a PR's watch then follows; denial may give a reason or name the
issue it duplicates. A proposed issue stays when your workspace is removed, so
proposing follow-ups before `shoal done` is fine. A pending PR proposal keeps
your workspace; new commits outdate it, so run `pr open` again after pushing
more work, and when the message says so.

Every link names its relation last: `assigned` for the work you are doing,
`opened` for an item you created another way, `related` for one the work relates to.
Register a PR opened another way with `shoal link pr <number-or-url> assigned`.
Assigned PRs accumulate per workspace; after `done`, cleanup waits until all have
merged and the merged set contains current HEAD, or the default branch has its
changes. `shoal unlink pr <number-or-url>
assigned` unlinks one PR; omit its number or URL to unlink all assigned PRs. Closed, unmerged PRs keep waiting, so cancel their
watches before finishing without a merge.

While watched PRs are open, Shoal delivers their activity to Claude Code, and to
Codex in a zmx session, launched through Shoal: as context after tool calls,
and after you end your turn by starting your next one with it. Handle each
delivery by reading its items with `shoal view <url>`, then end your turn to
wait; a delivery replaces a running watch. Other agents, Happy sessions
included, run `shoal --json watch pr` from the workspace, handle the returned
`updates` by reading their items with `shoal view <url>`, then wait again. Comments
and reviews are reported, as is each completed CI check or a merge conflict,
again after a push or base change that leaves it conflicting, but
not comments, closing or reopening you did through Shoal;
respond to available review findings while other checks run. The first wait
includes existing activity; later waits share a persistent cursor per workspace.
Run one wait at a time and read its result: a newer wait in the workspace
supersedes a running one, which returns `superseded: true` without updates.
`--timeout <seconds>` bounds the wait (1–3600 seconds); omitted, the wait is
unbounded. `timed_out`
with empty `updates` means no update. A `lookup_failed` entry names a failed
activity lookup and repeats every 10 minutes while it persists; until that source
recovers, check what it covers yourself, such as merge conflicts with
`shoal conflicts`. Waiting also
reports PR closure or merging and grants no permission to merge. A
`base_merged` update means your base workspace landed. It says whether Shoal
retargeted your PR; run the rebase command it gives and push with
`git push --force-with-lease`. For the current
state instead of changes, `shoal --json status` lists each watched PR in `prs`
with its state, merge conflicts, CI check results, review state and lookup errors.
`shoal view` adds the linked issue and each item's description, comments and
reviews with inline comments; use it instead of `gh` or `fj` to read them, with
`view pr <number>` or a URL for an unlinked item and `--no-comments` for state only.
`shoal view` automatically includes the log tails and URLs of failed CI jobs where
the forge reads job logs; checks from outside services have none.
`--check '<check name>'` selects only that check's log. Failed-check watch updates
include the failing step when the runner records it and a command to read the log.
`shoal --json ci rerun --check '<check name>'` reruns a failed check on the PR's
current head, after you have read its log and fixed or reported the cause, and
`shoal --json ci cancel` cancels running ones; omit `--check` for all of them.

`shoal done` defaults to cleanup, which may stop your execution once a grace
period (`[cleanup] grace_minutes`) has passed. Removal is reversible: the user
can reopen a removed workspace with `shoal restore`. Cleanup waits while one of
your questions is open, so ask with `shoal ask` when you need the user before the
workspace goes. `shoal done` closes your open questions, and `shoal undone` does
not reopen them. Holds belong to the user and host apps; you can list them with
`shoal hold` but not acquire or release them. `--cleanup` overrides a
`[done] cleanup = false` default. Dirty or newer work is retained;
completion without a PR watch also requires every commit pushed or on the local
default branch. Completion does not claim a merge occurred. Inspect
`completion.error` and `pr_cleanup.error` for cleanup or lookup failures.

When a linked PR is ready to merge (checks pass, review findings handled, no
conflicts), or the issue is solved without one, run `shoal ready`. It marks
your linked items, or the workspace when nothing is linked, ready for review at
HEAD for the user's integrations and takes a linked draft PR out of draft
unless `[ready] undraft = false`. When something new needs fixing, run
`shoal unready` before fixing it (`--draft` also returns the PR to draft), and
`shoal ready` again once it is fixed. To tell the user about status without
finishing, run `shoal notify "<one-line message>"`; if it stops applying before
the user reads it, withdraw it with
`shoal notifications withdraw <number>`, using the number it printed. Neither
affects completion or cleanup. Ask for decisions with `shoal ask` instead (see
Questions).

`shoal undone` withdraws a `shoal done` you ran too early. When the user gives
you more work during the grace period, run it before starting. It does not keep
the workspace.

## Checkpoints

`shoal --json checkpoint --name "<what comes next>"` keeps your workspace's HEAD
and unsaved files before a risky change; Shoal also takes one when you exit or
are stopped. `shoal --json restore <workspace>` lists them, and
`shoal --json restore <workspace> <number>` returns to one after keeping the
current state in a new checkpoint. It refuses while another command runs in the
workspace.

## Messages

The user and Shoal can send messages to your workspace, such as a request to
free memory or disk space. Claude Code and Codex launched through Shoal receive
them with PR activity, as described above; a new message also ends `shoal --json watch pr`
with a `messages` array, and `shoal --json messages` shows new ones. Each
message is shown once. Follow it.

## Questions

When you need the user to decide something, ask a question rather than
notifying:

```sh
shoal --json ask "Which database should the cache use?" --recommend SQLite \
  --reason "No server to run" --choice SQLite --choice Postgres
```

State the decision in one line and always recommend an answer, so the user can
reply with a word or a choice number; `--choice` is optional. The answer arrives
as a message naming the question, like any other message. Continue with other
work meanwhile, or end your turn to wait. When you cannot continue without the
answer, add `--wait`: it blocks without a time limit and prints the answer, so run
it in the background when your tool calls time out. Asking the identical question
again waits for the same one. Only the user answers, with `shoal answer`;
`shoal --json questions` lists your workspace's open questions. When you no
longer need an answer, withdraw the question with
`shoal --json questions withdraw <number>`. An open question keeps your
workspace from idle cleanup.

## Workspace context

Inside Shoal's tracked execution wrapper, commands inherit scope
and resolve to your own workspace, or to a child workspace you split from it
when run inside the child's directory. Agents launched independently in a managed
worktree can use the same commands: Shoal resolves the workspace from the current
directory, but the session itself has no execution scope or lifecycle tracking.

When unsure whether your checkout is managed, use `shoal --json work` and match
your working directory to a returned workspace `path`. Shoal's resources
require a managed workspace; do not select another agent's workspace or create
one just to obtain a lease.
If workspace setup failed, `shoal --json setup` reruns the configured setup
command and post-setup hook for the current workspace.

The daemon, its service and the Shoal installation belong to the user; stopping
or restarting the daemon stops every agent. Never administer them, clear Shoal
variables from your environment, or run a development build of Shoal against the
user's state directory. When a Shoal command is missing, refused or failing,
report the error with `shoal notify` and wait instead of working around it.

## Update from the default branch

```sh
shoal --json sync
shoal --json conflicts     # exit 1 lists the paths that conflict with main
git rebase main            # or git merge main; use your repo's branch name
```

`shoal sync` fetches your repository's remote and fast-forwards the local
default branch, advancing the registered checkout when it has that branch
checked out, where `git fetch origin main:main` is refused. It never pushes or changes your branch. Rebase or merge
with plain Git afterwards; other pushed branches are current as
`origin/<branch>`. `shoal conflicts` checks committed HEAD against your base
workspace's branch, otherwise the local default branch, or a branch you name,
without touching the worktree. Without a watched PR, `shoal --json watch`
returns a `merge_conflict` update when that check newly fails. After rebasing a pushed branch, push it with
`git push --force-with-lease`.

Branches of other workspaces are shared refs, so `git merge feature/api`
brings in their latest commits; use `origin/feature/api` for pushed work after
`shoal sync`. Stay on your workspace's recorded branch. When it builds on
another workspace's branch, record that with `shoal base set <workspace>`;
`shoal base` shows it.
When the repository has no remote, `shoal --json land` merges your committed
branch into the default branch. Unless the repository sets `[land]
requires_approval = false`, it first exits 2 with `approval_pending` for your
current HEAD: wait for the decision message, then run it again without new
commits. A new commit needs a new request; stop on `approval_denied`.

## Child workspaces

When part of the work belongs in its own PR, such as an unrelated fix you
found or one part of a large change, split it into a child workspace instead of
switching branches or creating worktrees yourself:

```sh
shoal --json split fix-typo                  # New branch from the default branch
shoal --json split --issue 42                # Named after the issue, which it links
shoal --json split api-part --base <branch>  # Stacked on your branch
```

The child gets its own worktree, branch and setup; the result gives its `path`.
Work there with `cd` or absolute paths: Shoal commands run inside the child's
directory act on the child, so `shoal pr open`, `shoal ready` and `shoal done`
there open, mark and complete the child's work. Your hooks keep reporting for
your own workspace, and the child's issue and PR activity reaches you like your
own, each update naming the child workspace. Run `shoal done` in each child once
its work is finished; Shoal cleans up your workspace only after its children are
removed. You can split children from children; each workspace process reaches
its own workspace and those below it.

To have another agent do the child's work in parallel, start one there:

```sh
shoal --json split --issue 43 --agent claude
shoal --json split api-part --agent codex -- "Move the API client into its own crate"
```

It starts from the agent queue, detached, as the child's own agent, and its
issue and PR activity goes to it rather than to you. You coordinate it: its
messages, questions, completion and ready marks, and its exits, reach you as
messages naming the child instead of reaching the user. Answer its questions
with `shoal answer <number> <answer>`, or ask the user yourself with
`shoal ask` when the decision is theirs. Send it instructions with
`shoal message <child> "<text>"`.

A child agent's approval request comes to you when you hold an approved grant
for the same thing; otherwise it passes you by, with a message saying so. Decide
it with `shoal approve <number>` or `shoal deny <number>`, or pass it up with
`shoal escalate <number>` when the user should decide.

## Ports

```sh
shoal --json leases port
shoal --json acquire port web
shoal release port web
```

Use configured names/defaults when available. Repeating a reservation returns the
same port. Pass `--reason "purpose"` for an ad hoc reservation.

Exit 2 with `reserved: false` means a conflict suggestion, not a reservation.
If the suggested port suits the task, accept by repeating the request with
`--port <suggested_port>`. Always use the returned `port`; don't assume the
preferred number was allocated. New reservations do not update your current
environment: pass the number to the server explicitly.

## Resource pools

```sh
shoal --json leases resource
shoal --json acquire resource devices --queue
shoal release resource devices
```

Use the returned `resource`; `--member <name>` requests a specific member.
Standalone resources use the same commands. Each semaphore lease consumes one permit.
The same `--name` returns the same lease; use distinct names for additional
permits; release without `--name` frees every lease in the pool. Busy requests exit 2. Actual use is cooperative.
`--queue` waits until the lease is granted or its approval denied, taking capacity
in arrival order; while others are queued, requests without it are busy. It has no
time limit: run it in the background when your tool calls time out, and keep it
running, since stopping it gives up the place.

For `kind: rwlock` resources, use `--mode read` for read-only access or `--mode write`
for changes (new leases default to write). Readers share; writers exclude everyone
on that resource. Check `read_available`/`write_available`, not just free slots.
Release before changing mode; there are no atomic upgrades.

## Simulators

```sh
shoal --json leases sim
shoal --json acquire sim --queue
shoal release sim
```

Acquisition uses configured preferences and returns an exclusive lease. Use its
`udid` explicitly with app/test tools, never simctl's ambiguous `booted` selector.
Let Shoal manage device creation, boot, shutdown, erase, and deletion. Exit 2
means capacity is busy; wait with `--queue` instead of retrying in a loop, and
don't bypass allocation.

Normal reuse preserves apps, data, and settings. Only request a clean device
when the task specifically requires pristine state:

```sh
shoal --json acquire sim --clean --reason "Verify first-launch permission prompts"
```

Give the actual task-specific reason; clean requests and their outcomes are
audited. Release an existing lease before requesting it clean. Shoal chooses
the device to minimize reinstalls. Only installed runtimes are supported.

Stop servers, app automation, and debuggers before releasing their resources.
Release when finished; reservations and leases outlive the requesting command.

## Secrets

A tool whose secret requires approval fails with the request to make. Run
`shoal --json acquire secret NAME --reason "purpose"`, give the user the request
ID, and rerun the tool once it is approved. The value never reaches your
environment; `shoal release secret NAME` gives up the grant.
