# Reviews

This file says what the `review` commands do, for local reviews and for your own
reviews of a space.

## Local reviews

`shoal review run [space]` queues a review of the workspace's committed HEAD
against [`shoal diff`](branches.md#diff)'s base. An agent reviews it once, in
the background and without a terminal. It only reads, and it reads a checkout
of that commit, so uncommitted edits and later commits stay out. `review run`
can also run a review tool instead; see the
[review commands](run.md#review-commands).

### The review checkout

The checkout is a detached checkout in the repository directory's `.reviews`,
outside the workspace. No setup runs in it. Each workspace, and each PR reviewed
in it, has one place there, so every review of it checks its commit out at the
same path. The checkout's files are read-only, whatever the reviewer's settings
allow, and Shoal adds no trust entry for it. The daemon reads the workspace's
configuration only once its worktree verifies as the workspace's.

Shoal removes the checkout when the review ends. A checkout that cannot be
removed stays until `shoal daemon restart`. Until then, later reviews of its
workspace or PR run beside it, each in a new session, and the repository cannot
be moved or removed. When the processes of its reviewer cannot be proven
stopped, it also keeps counting toward the memory and disk that starts need.

### Queued reviews

A workspace has at most one queued or running review. Running `review run`
again returns the queued one, and is refused while one runs. A queued review
takes the workspace's HEAD when it starts.

A review waits in the [agent queue](queue.md) as an agent's start does, but
`shoal queue` does not list it. When the machine goes to sleep, Shoal stops each
running reviewer and queues its review again, which starts over once the
machine wakes. A daemon that starts again queues each review that was running.

A workspace's package, which [`shoal move`](move.md) and the [hub](hub.md)
carry to another machine, holds its queued or running review. The machine that
continues the workspace queues that review again, with the same agent, model
and target.

### The reviewer

The reviewer is `--agent`, else
[`[review] agent`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#reviews),
else `agent.default`, else one you pick. Its AI tool needs a
[`review` template](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#ai-tools).
It runs with `--model` and `--effort` over its
[configured model and effort](agents.md), and keeps them when the review starts
over. The fallback agent runs with its own configured ones.

The reviews of a workspace, and those of each PR reviewed in it, continue one
session. A review resumes the session that the latest earlier review saved,
through the tool's
[`review_resume` template](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#ai-tools),
when the same agent saved it. Another agent, such as the fallback, starts a new
session, and so does a session that cannot be resumed. Only the AI tool's
conversation carries over, and the session goes with the workspace.

### How the reviewer writes a review

The reviewer writes its review with Shoal's commands. Only its run holds its
scope, a scope of the reviewed workspace. With it, the reviewer reads what that
workspace's scope reaches and changes only its review. Shoal refuses every
other change.

- `shoal thread open "<title>" --code PATH:FIRST-LAST --severity
  blocker|should-fix|nit --body "<body>"` opens a finding on lines of the
  checkout. The lines point at the reviewed commit, trimmed to the file. A path
  outside the checkout is refused.
- `shoal send <thread> "<why>"` in a finding that the workspace holds and
  resolved `fixed` reopens it once the review is submitted. The reviewer can
  post nothing else.
- `shoal review submit --body "<summary>"` finishes the review at the commit its
  run reviewed. `--approve` approves. The verdict follows the rule for
  [your own reviews](#submit-a-review).

### Failed reviews

A run that exits without submitting fails the review as `not-submitted`, unless
its result or diagnostics name a failure first. Once a run has submitted, how it
exits changes nothing. A reviewer still running after 30 minutes is stopped, and
the review fails as `timed-out`. After any other failure,
[`[review] fallback_agent`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#reviews)
reviews once more.

Shoal drops what a run that did not submit opened and reopened. It does so
before the fallback runs, before the review fails, and before the review runs
again after protection, the machine's sleep or a daemon restart.

A failed review records why it failed as one of `usage-limit`,
`quota-exhausted`, `rate-limit`, `authentication-failed`, `execution-failed`,
`not-submitted`, `timed-out` or `invalid-false-positives`. It never records the
reviewer's output.

### What the reviewer reads

The [`review` rule](modes.md) gives the instructions. Each review also receives
the workspace's earlier reviews as context. Each earlier review comes with the
threads of its findings, each with its posts and outcome. A finding resolved
`fixed` also lists the files that the changes
[attributed to it](threads.md#agents-changes) touched. The review of a PR also
receives the PR's description, conversation and reviews.

The reviewer checks every finding resolved `fixed`. When the problem is still
present, it reopens the finding's thread with a post saying why. It raises a
settled claim again only when the claim's reason no longer holds.

### False positives

Shoal adds a finding resolved `false-positive` to `.shoal/false-positives.toml`
in the repository's own directory, such as
`~/shoal/my-project/.shoal/false-positives.toml`. The file sits beside the
repository's [local config](config.md#store-repository-config-outside-git),
outside every Git tree.

Every later review of any of the repository's workspaces receives the file's
entries, newest first, up to 32 KiB of them. The reviewer skips a claim while
its reason holds, and reports the claim when the change invalidates the reason.
While the file is invalid, reviews of the repository fail as
`invalid-false-positives`. Fix or delete the file to end that.

Reopening the finding, or resolving it another way, removes its entry. Removing
its workspace does not. Edit the file to remove an entry or add one:

```toml
[[false_positive]]
finding = "17.2"            # The finding's review and its place there; omit for an entry you add
claim = "The length is unchecked"
path = "src/parse.rs"
reason = "The caller checks the length"
```

### Findings

Each finding is a [thread](threads.md) of the `review` kind that Shoal opens for
the review. The reviewed workspace holds it, and it is numbered in that
workspace's space, such as `3.4`. The finding's title names the thread. Its body
is the opening post, which points at the finding's lines in the reviewed
commit. The thread follows those lines as the code changes. The finding's
severity, `blocker`, `should-fix` or `nit`, shows with it.

The holder hears of a finding only once the review is submitted. When a review
ends, the workspace's agents receive it as one message, which wakes an idle
agent. For a finished review, the message gives the summary and each thread the
review opened or reopened. It ends with a note that the reviewer is automated
and can be wrong. For any other review, it says why the review failed or that it
was cancelled. The threads' own posts announce nothing more.

A workspace whose own process cancelled the review is not told. The agent or
chat that coordinates the workspace hears one line, such as
`fix-login: review 17 finished: 1 blocker, 2 should-fix`.

### Follow reviews

```sh
shoal review ls [space]       # Newest first; --repo R for a repository's
shoal view review [ID]        # One review in full; default: the workspace's latest not pending
shoal watch review [ID]       # Wait until it ends, then show it
shoal review cancel [ID]      # Default: the space's pending, queued or running review, else your only pending one; --space S
```

`review ls` marks a review outdated once the workspace's HEAD has moved past it.
It counts the open threads that each review opened or reopened. `view review`
shows the run with each of its threads and their posts. `watch review` keeps
waiting through a daemon restart. After an upgrade changes the protocol, it
continues from the installed binary. Plain `shoal view` in a workspace ends with
its latest review, and `shoal list` shows each workspace's latest review state.
Every command takes `--json`.

A workspace process runs and cancels the reviews of its own workspace and of
the workspaces below it. It resolves the threads its workspace holds. Removing
a workspace cancels its review and deletes its reviews.

### Resolve findings

```sh
shoal thread switch 3.4         # Changes from now on are for the finding
shoal send 3.4 'It only happens on empty input'  # The reviewer answers in the thread
shoal thread resolve 3.4 false-positive --reason 'The caller checks the length'
shoal thread resolve 3.4 --undo
```

The workspace's agent, or you, resolves a finding's thread with `shoal thread
resolve` and one outcome with its data:

| Outcome | Meaning | Takes |
|---|---|---|
| `fixed` | The code was changed to address it; the next review checks it | — |
| `false-positive` | The claim is wrong | `--reason` |
| `wont-fix` | The claim holds, but the code stays | `--reason` |
| `deferred` | The claim holds and other work fixes it | `--issue` |
| `duplicate` | Another thread makes the same point | `--of <thread>` |
| `withdrawn` | It no longer applies | `--reason`, optional |

Fix a finding after `shoal thread switch` to its thread. The changes made then
belong to the thread, and the next review checks the finding against them.

A finding that needs your decision stays unresolved while the agent asks you in
a [thread](threads.md) of its own. Until you answer, the
[review gates](#review-gates) stay closed.

### Ask the reviewer

A post in a finding's thread reaches the reviewer, whether the holder or you
sends it, as in `shoal send 3.4 "<text>"`. Shoal resumes the session that the
reviews of the workspace or PR continue. The reviewer runs once, read-only, in
a checkout of the workspace's latest commit, through the tool's `review`
template. It posts its answer in the thread with `shoal send`, which tells the
holder. When it posts none, Shoal posts why. A post that waits for an answer
still gets one after a daemon restart.

### Review gates

The `require_for_` keys of
[`[review]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#reviews)
make commands wait for a review. The configuration reference says whose
commands each value holds.

- `require_for_pr` checks a `shoal pr open` that would open a new PR. It checks
  before the push and before a
  [proposal](approvals.md#approval-for-agent-issues-and-prs). Linking an
  existing PR is not checked.
- `require_for_ready` checks `shoal ready`, but not `ready --undo`.
- `require_for_merge` checks a local
  [`shoal merge`](branches.md#local-merges) for the commit of
  the branch that merges. It checks a
  [PR merge](issues.md#issue-and-pr-actions), with or without `--auto`, for the
  PR's head, which the merge then names to the forge.

A PR merge's gate reads the reviews of the workspace that
`shoal review run <url>` reviews the PR in: the one that owns the PR's branch,
or for a fork's PR the one at a commit of it. Without such a workspace, the
merge is refused, naming `shoal review run <url>`. For an unregistered
repository's PR, the refusal says to merge on the forge instead. A merge the
forge cannot hold to that head is refused too, such as `--auto` where
`shoal forge capabilities` lists `auto_merge_at_head` as unsupported.

A merge's gate follows the config of the repository it merges into: its
checkout's file, its local config and the global config. It never follows the
merging branch's own file. For an unregistered repository's PR, it follows the
global config. The gate checks a merge before the merge asks its question and
before an agent's approval request, so a refused merge never makes one.

A gate passes once a finished review covers the commit exactly and every thread
that review opened or reopened is resolved, whatever the verdict. A
[review you or a space wrote](#your-own-reviews) covers the commit it recorded
when it was submitted. One without comments passes the gate only when it
approves, and one that neither comments nor approves never decides a gate.

Any review of the commit with unresolved threads decides over the others, so
no other review of that commit opens the gate.

Otherwise the command is refused. The refusal names what is missing and lists
the unresolved threads. When the commit has no review and none is queued or
running, the refusal also queues a review of the workspace's HEAD, where HEAD is
that commit and the caller's scope reaches the workspace. That review's
findings arrive as a message. A PR merge refused otherwise names
`shoal review run <pr-url>`, which reviews the PR in the workspace that owns
its branch. A gate's review is a workspace review, so it is never posted.

### Review a PR

`shoal review run <pr-url>`, or `--pr <number-or-url>`, reviews a PR's head
against its base. Shoal looks up the PR with your [token](issues.md#forges) and
fetches both from origin. `--repo` selects the PR's repository, which otherwise
follows `shoal start`. An issue URL is refused: a
[plan](modes.md#planning-issues) settles an issue before implementation.

The review runs in the workspace that owns the PR's head branch, fast-forwarded
to the pushed head. It is refused when the branch has diverged, or when the
worktree is no longer the recorded one on that branch. Without such a
workspace, Shoal opens one from origin whose base is the PR's `origin/<base>`.
Either workspace links the PR as `shoal start` does, and keeps the review.

A fork's PR is reviewed in its workspace at a commit, which Shoal opens or moves
as `shoal start` does, but with `--skip-setup`. Its review covers the PR's head
against its base, whatever was committed on top there. A manual review reads
the checkout, so it refuses a workspace with commits on top of the PR's head.

Unless
[`[review] post`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#reviews)
is false, Shoal posts a finished PR review on the PR. It posts a comment review
as the workspace's agent account, as
[`shoal pr review --comment`](issues.md#issue-and-pr-actions) submits one:

- Findings on lines that the PR's diff shows become line comments.
- The other findings go in the body with the summary.
- A review with findings ends with the automated-review note.

A posted review never approves or requests changes. When the PR's head moved
while the reviewer ran, Shoal still posts the review for the commit it covered,
with every finding in the body. `--post` or `--no-post` overrides the setting
for one review, and also selects an agent reviewer. Workspace reviews are never
posted, even with a linked PR. Reviews on the forge stay there, and Shoal does
not mirror them.

### Chat reviews

A [chat](chats.md) has no changes of its own. `review run` from a chat, or
naming one, reviews the chat's one linked PR as `--pr` would. A chat's reviews
are those of its linked PRs: `review ls` lists them, and `review cancel`
cancels the one queued or running. The workspace that owns the PR's branch
holds their threads.

## Your own reviews

```sh
shoal review start [space]                           # A pending review of a workspace's work
shoal thread open "Retries are unbounded" --code src/fetch.rs:40-52 \
  --severity should-fix --body "Cap them at the configured limit."   # Its comment
shoal review submit [--approve] [--body T | --body-file F] [-s space]
shoal review cancel                                  # Drop it with its comments
```

You, or a space's agent, review a workspace's work as the
[local reviewer](#local-reviews) does. A space reviews the workspaces below it,
never its own.

### Write comments

`review start` starts your pending review of the workspace, or returns the one
you have. Each writer has one pending review per workspace.

While your review is pending, a [thread](threads.md) you open towards that
workspace without `--kind` is its comment. From a space, name the workspace
with `--space`. A comment is a `review` thread with a number, which your editor
shows. Shoal numbers it in your space. When you are the user, or the workspace
runs on another machine than your space, Shoal numbers it in the workspace
instead, and that machine keeps the review and its comments. `--kind question`
or `--kind request` opens a thread outside the review, delivered at once.

Until you submit, a comment's posts reach nobody and keep no one waiting. Only
you, the spaces that reach the writing space and the user read it, in threads,
in the review and in logs. Its holder neither posts in it, forwards it nor
resolves it. A comment's `--code` may point at unsaved lines.

`--severity blocker|should-fix|nit` marks how much a comment matters, as the
local reviewer marks its findings. The review's tallies, its delivery and a
posted PR review count it so. A thread that joins no review refuses
`--severity`.

### Reviews on other machines

`review submit` and `review cancel` take your only pending review without `-s`.
Shoal looks for it on every machine it can reach. While a machine cannot be
reached, a thread without `--kind` towards one of its workspaces is refused.
`--kind question` opens a question instead.

### Submit a review

`review submit` submits your pending review of `--space`, else of the space it
runs in, else your only one. It records the workspace's HEAD. `--body` or
`--body-file` gives a summary of up to 4 KiB. The workspace's agent receives the
summary and each comment as one message. The agent coordinating the workspace
hears one line of a review you wrote.

The verdict is `approve` with `--approve`. Otherwise it is `request_changes`
when the review has comments, and `comment` when it has none. The
[review gates](#review-gates) say when a submitted review counts.

Once submitted, comments work as any thread does. The agent switches to a comment's
thread before it fixes the comment, so its changes are attributed there, and
says so in the thread. You check the changes and resolve the thread with an
outcome from [the table](#resolve-findings), or reply. You resolve your
comments: a holder resolves only the threads Shoal opens.

### Cancel a review

`review cancel` drops a pending review with its comments. Its writer or you can
cancel it. Without a review ID or `--space`, it acts on the space it runs in.
Where that space has no review to cancel, or outside every space, it drops your
only pending review.

Removing the reviewed workspace, or the space that writes the review, drops a
pending review, on whichever machine keeps it.
