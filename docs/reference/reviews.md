# Reviews

This file says what the `review` commands do, for local reviews and for your own
reviews of a space.

## Local reviews

`shoal review run [space]` queues a review of the workspace's committed HEAD
against [`shoal diff`](branches.md#diff)'s base. An agent reviews it once in the
background, without a terminal, read-only, in a detached checkout of that commit
in the repository directory's `.reviews`, outside the workspace, so uncommitted
edits and later commits stay out; no setup runs. Each workspace, and each PR
reviewed in it, has one place there, so every review of it checks its commit out
at the same path. The checkout's files are read-only whatever the reviewer's
settings allow, and Shoal adds no trust entry for it; it is removed when the
review ends. A checkout that cannot be removed stays until `shoal daemon restart`;
until then, later reviews of its workspace or PR run beside it, each in a new
session, and the repository cannot be moved or removed; one whose reviewer's processes
cannot be proven stopped also keeps counting toward the memory and disk that starts need. The daemon reads the workspace's configuration only once its
worktree verifies as the workspace's. A queued review takes the
workspace's HEAD when it starts. A workspace has at most one queued or running
review: running the command again returns the queued one, and is refused while one
runs. A review waits in the [agent queue](queue.md), ranked
with the workspace it reviews, for the memory and disk an agent's start needs,
but takes no heavy slot; `shoal queue` does not list it. A review that
the machine's sleep or a shutdown cuts off runs again: as the machine goes to
sleep, Shoal stops each running reviewer and queues its review again, which
starts over once the machine wakes, and a daemon that starts again queues
each review that was running. A workspace's package, which
[`shoal move`](move.md) and the [hub](hub.md) carry
to another machine, holds its queued or running review, which the machine
that continues the workspace queues again, with the same agent, model and
target. A
chat has no changes of its own; see [chat reviews](#chat-reviews) below.

The reviewer is `--agent`, else [`[review] agent`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#reviews), else `agent.default`, else one you
pick, and needs a [`review` template](agents.md). It runs with `--model` and `--effort`
over its [configured model and effort](agents.md), which the review keeps if it starts
over; the fallback agent runs with its own configured ones. The reviews of a
workspace, and those of each PR reviewed in it, continue one session: a review
resumes the session the latest earlier one saved, through the tool's
[`review_resume` template](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#ai-tools), when the same agent saved it,
so the reviewer keeps what it understood and its cached prompt. Another
agent, such as the fallback, starts a new session, as does a session that
cannot be resumed; the session goes with the workspace; only the AI tool's
conversation carries over.

The reviewer writes its review with Shoal's commands, under a scope of the
reviewed workspace that its run alone holds: it reads what that workspace's
scope reaches, and changes only its review. `shoal thread open "<title>" --code
PATH:FIRST-LAST --severity blocker|should-fix|nit --body "<body>"` opens a
finding on lines of its checkout, which point at the reviewed commit, trimmed to
the file; a path outside the checkout is refused. `shoal send <thread> "<why>"`
in a finding the workspace holds resolved `fixed` reopens it once the review is
submitted, and no other post is taken. `shoal review submit --body "<summary>"`,
with `--approve` when it approves, finishes the review at the commit its run
reviewed, with the verdict a [written review](#your-own-reviews)'s submit gives.
Every other change is refused. A run that exits without submitting fails the
review as `not-submitted`, unless its result or diagnostics name a failure
first; once it submitted, how it exits changes nothing. A reviewer still running
after 30 minutes is stopped and the review fails as `timed-out`; after any other
failure, `fallback_agent` reviews once more. What a run that did not submit
opened and reopened is dropped before the fallback runs, the review fails, or it
runs again after protection, the machine's sleep or a daemon restart. A failed
review records why as one of `usage-limit`, `quota-exhausted`, `rate-limit`,
`authentication-failed`, `execution-failed`, `not-submitted`, `timed-out` or
`invalid-false-positives`, never the reviewer's output.

The [`review` rule](modes.md) gives the instructions. Each review also
receives, as context, the workspace's earlier reviews with the threads of their
findings, each with its posts, outcome and, for one resolved `fixed`, the files
the changes [attributed to it](threads.md#agents-changes) touched, and for a PR its description,
conversation and reviews. It checks every finding resolved `fixed`, and reopens
the thread of one whose problem is still present with a post saying why; it
does not raise a settled claim again unless its reason no longer holds.

A finding resolved `false-positive` is added to `.shoal/false-positives.toml` in the
repository's own directory, beside its [local config](config.md#store-repository-config-outside-git)
and outside every Git tree (`~/shoal/my-project/.shoal/false-positives.toml`). Every
later review of any of the repository's workspaces receives its entries, up to 32 KiB
of them, the newest first. The reviewer skips a claim while its reason holds and
reports it when the change invalidates the reason. While the file is invalid, reviews of
the repository fail as `invalid-false-positives` until it is fixed or deleted. Reopening the finding or resolving
it another way removes its entry; removing its workspace does not. Edit the file to
remove an entry or add one:

```toml
[[false_positive]]
finding = "17.2"            # The finding's review and its place there; omit for an entry you add
claim = "The length is unchecked"
path = "src/parse.rs"
reason = "The caller checks the length"
```

Each finding is a [thread](threads.md) of the `review` kind that Shoal opens for the
review, held by the reviewed workspace and numbered in its space, such as `3.4`:
its title names the thread, and its body is the opening post, pointing at its
lines in the reviewed commit, which the thread follows as the code changes. Its
severity, `blocker`, `should-fix` or `nit`, is shown with it. Its holder hears
of it only once the review is submitted. When a review ends, the
workspace's agents receive it as one message, waking an idle agent: a finished
review's summary with each thread it opened or reopened, ending with a note that
the reviewer is automated and can be wrong, or why it failed or that it was
cancelled; the threads' own posts announce nothing more.
A workspace whose own process cancelled the review is not told. The agent or chat
coordinating the workspace hears one line, such as `fix-login: review 17
finished: 1 blocker, 2 should-fix`.

```sh
shoal review ls [space]       # Newest first; --repo R for a repository's
shoal view review [ID]          # One review in full; default: the workspace's latest not pending
shoal watch review [ID]         # Wait until it ends, then show it
shoal review cancel [ID]        # Default: the space's pending, queued or running review, else your only pending one; --space S
shoal thread switch 3.4         # Changes from now on are for the finding
shoal send 3.4 'It only happens on empty input'  # The reviewer answers in the thread
shoal thread resolve 3.4 false-positive --reason 'The caller checks the length'
shoal thread resolve 3.4 --undo
```

`watch review` keeps waiting through a daemon restart, and continues from the
installed binary after an upgrade changes the protocol. `view review` shows the
run with each of its threads and their posts. `review ls` marks a review
outdated once the workspace's HEAD has moved past it and counts the threads it
opened or reopened that are open. Plain `shoal view` in a workspace ends with
its latest review, and `shoal list` shows each workspace's latest review state.
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

A finding fixed after `shoal thread switch` to its thread has the changes made
for it in the thread, which the next review checks it against. A post in a
finding's thread, `shoal send 3.4 "<text>"` from its holder or you, reaches the
reviewer: Shoal resumes the session the reviews of the workspace or PR continue,
as a one-shot, read-only run in a checkout of the workspace's latest commit,
through the tool's `review` template, which posts its answer in the thread for
Shoal with `shoal send`, telling the holder; when it posts none, Shoal posts
why.
A post that waits for an answer gets it after a daemon restart too. A finding that
needs your decision stays unresolved while the agent asks in a
[thread](threads.md) of its own, which keeps the gates below closed until you
answer.

With [`[review] require_for_pr`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#reviews), a `shoal pr open` that would open a new
PR is checked before it pushes, and before a [proposal](approvals.md#approval-for-agent-issues-and-prs); linking an
existing PR is not. With `require_for_ready`, `shoal ready` is checked, not
`ready --undo`. With `require_for_merge`, a local [`shoal merge`](branches.md#local-merges) is checked for the
commit of the branch that merges, and a [PR merge](issues.md#issue-and-pr-actions), with or without
`--auto`, for the PR's head, which the merge then names to the forge, against the
reviews of the workspace `shoal review run <url>` reviews it in: the one that
owns the PR's branch, or for a fork's PR the one at a commit of it. A PR with no
such workspace is refused, naming `shoal review run <url>`, or for an
unregistered repository's PR, to merge on the forge, as is a
merge the forge cannot hold to that head, such as `--auto` where `shoal forge
capabilities` lists `auto_merge_at_head` as unsupported. A merge's gate follows
the config of the repository it merges into, its checkout's file, local config
and global config, never the merging branch's own file, and for an unregistered
repository's PR the global config. A merge is
checked before the question it asks and before an agent's approval request,
which a refused merge never makes. Each gate holds agents and the commands they run with `"agents"`,
and your own commands too with `"all"`. A gate passes once a finished review covers HEAD exactly and every
thread it opened or reopened is resolved, whatever the verdict, except that [one you or a
space wrote](#your-own-reviews) without comments passes only when it approves,
one that neither comments nor approves is passed over, and any review of the
commit with unresolved threads decides over the others. Otherwise the command is
refused, naming what is missing and listing the unresolved threads; without a
review of the commit and with none queued or running, it queues a review of the
workspace's HEAD, whose findings arrive as a message, where that is the commit and
the caller's scope reaches the workspace. A PR merge refused otherwise names
`shoal review run <pr-url>`, which reviews the PR in the workspace that owns its
branch. A gate's review is a workspace review, never posted. A workspace process runs and cancels the reviews of
its own workspace and those below it, and resolves the threads its workspace holds. Removing a workspace cancels its review and
deletes its reviews. Every command takes `--json`.

`shoal review run <pr-url>` or `--pr <number-or-url>` looks up the PR with your
[token](issues.md#forges) and reviews its head against its base, both fetched from origin, in
the workspace that owns its head branch, fast-forwarded to the pushed head and
refused when diverged or when the worktree is no longer the recorded one on that
branch, or in one opened from origin whose base is the PR's `origin/<base>`. Either
workspace links the PR as `shoal start` does, and the review is stored on it. A
fork's PR is reviewed in its workspace at a commit, opened or moved as `shoal start`
does but with `--skip-setup`, since the reviewer only reads the checkout; its
review covers the PR's head against its base, whatever was committed on top there,
and a manual review, which reads the checkout, refuses a workspace with commits on
top of the PR's head. `--repo` selects the PR's repository, which otherwise follows
`shoal start`. An issue URL is refused: `shoal start --mode plan --issue` settles an
issue before implementation. Unless `[review] post` is false, a finished PR review is posted on
the PR as a comment review by the workspace's agent account, as
[`shoal pr review --comment`](issues.md#issue-and-pr-actions) submits one: findings on lines the
PR's diff shows become line comments, the rest go in the body with the summary,
and a review with findings ends with the automated-review note. It never approves
or requests changes. When the PR's head moved while the reviewer ran, the review is
still posted for the commit it covered, with every finding in the body.
`--post` or `--no-post` overrides the setting for one review and selects an agent
reviewer. Workspace reviews are never posted, even with a linked PR.

<a id="chat-reviews"></a>A [chat](chats.md) has no changes of its own, so `review run` from a chat, or
naming one, reviews the chat's one linked PR as `--pr` would. A chat's reviews
are those of its linked PRs: `review ls` lists them, and `review cancel`
cancels the one queued or running. Their threads are held by the workspace that
owns the PR's branch.

## Your own reviews

```sh
shoal review start [space]                           # A pending review of a workspace's work
shoal thread open "Retries are unbounded" --code src/fetch.rs:40-52 \
  --severity should-fix --body "Cap them at the configured limit."   # Its comment
shoal review submit [--approve] [--body T | --body-file F] [-s space]
shoal review cancel                                  # Drop it with its comments
```

You, or a space's agent naming a workspace below it, review a workspace's work
as the [local reviewer](#local-reviews) does. `review start` starts your pending
review of the workspace, or returns the one you have; each writer has one per
workspace, and a space reviews the workspaces below it, never its own. While
it is pending, a [thread](threads.md) you open towards that workspace without
`--kind`, with `--space` from a space, is its comment: a `review` thread with
its number, which your editor shows: numbered in your space, or in the
workspace when you are the user or it runs on another machine than your
space, which then keeps the review and its comments; your only pending
review, which `review submit` and `review cancel` take without `-s`, is the
only one on the machines that can be reached. While that machine cannot be reached, a thread
without `--kind` towards one of its workspaces is refused, since only that
machine knows whether it joins a review; `--kind question` opens a question. Its posts reach
nobody and keep no one waiting: until you submit, only you, the spaces that reach the writing space and
the user read it, in threads, in the review and in logs, and its holder neither posts
in, forwards nor resolves it. `--kind
question` or `request` opens a thread outside the review, delivered at once. A
comment's `--code` may point at unsaved lines, since a reference keeps its own
file version. `--severity blocker|should-fix|nit` marks how much a comment
matters, as the local reviewer marks its findings: the review's tallies, its
delivery and a posted PR review count it so. A thread that joins no review
refuses it.

`review submit` submits your pending review of `--space`, else of the space it
runs in, else your only one, at
the workspace's HEAD, with a summary of up to 4 KiB from `--body` or
`--body-file`: the workspace's agent receives it as one message with each
comment, and the agent coordinating the
workspace hears one line of a review you wrote. Its verdict is `approve` with
`--approve`, else `request_changes` with comments, else `comment`. Comments then
post as any thread does: the agent fixes, switching to the comment's thread so
its changes are attributed there, and says so in the thread; you check them and
resolve it with an outcome of the table above, or reply. Only threads Shoal opens
are resolved by their holder. `review cancel` drops a pending review with its
comments, by its writer or you; without a review or `--space` it acts on the
space it runs in, and drops your only pending review where that space has no
review to cancel or outside every space. Removing the workspace a review is
of, or the space that writes it, drops it, on whichever machine it is kept.
For the review gates above, a submitted review covers the commit it recorded
once every comment is resolved, and one without comments only when it
approves; one that neither comments nor approves never decides a gate, and
while any review of the commit has unresolved threads no other review of it
opens one. Reviews on the forge
stay there; they are not mirrored.
