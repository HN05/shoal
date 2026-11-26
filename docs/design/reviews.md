# Reviews

This file covers `shoal-reviews`: local reviews, the reviews a space writes,
the review gates and the gate agent. [Reviews](../reference/reviews.md) and
[The gate](../reference/approvals.md#the-gate) say what each does; this file
gives the reasons.

## Local reviews

### The reviewer reads the committed HEAD

A local review judges what the agent committed. The daemon runs the reviewer
once, read-only, in a detached checkout of the workspace's committed HEAD. So
the reviewer never sees or changes the agent's uncommitted work.

### One session per workspace and PR

The reviews of a workspace continue one reviewer session, and so do those of
each PR reviewed in it. A later review thus keeps what the reviewer understood
and reuses its cached prompt. An AI tool finds a saved session by the
directory it ran in, so each workspace and PR has one checkout path.

The session adds context and decides nothing. Each review receives its
instructions and its history in full. A change of agent starts a new session.
The history is context, not instructions.

### The reviewer writes with everyone's commands

The reviewer writes its review with the commands that every review uses:
`thread open` opens a finding, `send` reopens one, and `review submit`
finishes the review. It runs under a scope that ends with its run and writes
only that review. A finding's lines in the checkout resolve to the reviewed
commit.

### A finding is a thread

Each finding is a `review` [thread](coordination.md#threads) that Shoal opens and the
reviewed workspace holds, not a record of its own. A finding thus reads,
resolves, follows the code and shows in editors as every thread about code
does, and its discussion is posts. The review record keeps only the target,
commit, reviewer, verdict and state.

### A submit delivers the whole review

A finding holds its posts until the review is submitted. The submit then
delivers the summary and every thread in one alert, so a review wakes the
agent once. A reopen waits for the submit too. So the holder sees nothing of a
run that never submits.

A run that exits without submitting fails the review. Shoal drops what that
run wrote before the review fails, before its fallback runs and before it runs
again.

### Outcomes are typed

The holder resolves a thread Shoal opened with a typed outcome, which carries
its reason or issue. A `fixed` outcome points at the changes attributed to the
thread. The next review receives those changes with the finding and reopens it
with a post saying why when the problem remains. So no review rereads free text
to judge a fix.

### The agent can ask the reviewer

A holder's post in a finding's thread, instead of a resolution, asks the
reviewer that made it. Shoal resumes the reviewer's session for that workspace
or PR as a one-shot, read-only run. The run answers in the thread with `send`,
under a scope that ends with the run and posts only there. A run that posts
nothing fails as a review's run does, and Shoal posts why.

Shoal reads from the thread's posts whether it waits for an answer. So a
pending question outlives a daemon restart.

### False positives belong to the repository

A finding resolved `false-positive` outlives its workspace as a claim of its
repository. Every later review of the repository receives it. So a wrong claim
that the reviewer would raise in every change to the same code is answered
once.

The claims live in a file beside the repository's
[local config](config.md#local-config), not in a tracked file. The outcome records a claim
without a command or commit of its own, and reopening the finding withdraws
it. The user edits the file to drop or add a claim.

### Failures are typed

A failure is a typed reason, classified as the review bot classifies its own.
So a backend's output never reaches a message.

A review's time limit is fixed at 30 minutes. A review that runs that long has
stopped making progress.

## One-shot agent runs

### What a run is

Reviews, answers to findings and the gate run as one-shot agent runs. A run is
an AI tool's command started without a terminal, in a process group of its
own. It may resume a session that an earlier run reported. Shoal kills it on a
timeout, a cancel or a daemon stop, and finds it again by its marker after a
crash.

The [agent queue](queue.md) admits each run ahead of the agent it serves,
which waits on it.

### Protection stops a run first

Protection stops a one-shot run before any agent, since a run has no running
session to lose. The run then asks for admission again and waits for healthy
readings, as a stopped agent's restore does.

### A review survives sleep, restarts and moves

A review that the machine's announced sleep would cut off stops in the same
way as the machine goes to sleep. It asks for admission again, which the
asleep queue gives once the machine wakes, without waiting for readings. A
daemon that starts again queues each review that was running.

A workspace's package carries its queued or running review. So the machine
that continues the workspace, after a move, a hand-over or a takeover, queues
the review again. The machine it left removes its copy, review and all.

## Reviews of PRs and chats

### A PR is reviewed in its workspace

A review of a PR reviews its head branch against its base, in the workspace
that owns the branch. That workspace's reviews and gates then cover the PR. A
scoped caller reads the PR as the agent account, and the user with their own
logins.

Shoal fast-forwards the workspace to the pushed head. Local commits ahead of it
stay, and a diverged branch is refused. The fast-forward runs under a lifecycle
reservation and the repository's Git gate. Shoal first verifies the recorded
worktree and its branch, so it never moves a switched or replaced checkout.

A fork's PR opens its workspace at a commit, without setup, since the reviewer
only reads the checkout.

### A chat reviews its linked PR

A chat has no worktree or branch, so it has no changes of its own to review. It
names the workspace it reviews, or reviews its one linked PR. A chat's reviews
are those of its linked PRs.

## Reviews a space writes

### Every review works the same

The user, or an agent within its scope, reviews a workspace's work as the
reviewer does. So what the agent receives, and what the gates count, does not
depend on who reviewed.

### A pending review collects comments

A written review is a review record that names its writer, who writes it
pending. A thread the writer opens towards the workspace without a kind joins
it as a `review` thread. Its posts wait, and it is numbered so editors show
it. The submit delivers the comments with the summary in one alert, as a local
review's findings arrive, and records the workspace's HEAD.

A thread joins the review by the review being pending, not by a `--kind`. So
an editor's first comment can start the review and a submit action can send
it. An explicit kind opens an ordinary thread outside the review.

A comment keeps its own version of the file, so it may point at unsaved lines.

### The writer resolves its comments

The writer, as the opener, resolves each comment after it checks the changes
attributed to it. Only the threads Shoal opened are the holder's to settle.
Shoal's reviewer neither reads nor reopens these comments, and learns no false
positives from them.

### Removal drops a pending review

Removing the reviewed workspace, or the writer's space, drops a pending review.
A restore could never submit its comments.

## Review gates

### A gate counts what a review raised

A gate counts the threads that a review opened or reopened. A reopened finding
is one that review raised again.

### Which review covers a commit

A finished local review covers its commit once those threads are resolved. A
written review covers its commit once its comments are resolved. One without
comments covers it only when it approves. So a summary alone neither opens a
gate nor closes one that a covering review opened.

Any review of the commit with unresolved threads decides the gate over the
others, since a later review settles none of them. An approval thus leaves the
reviewer's findings to be resolved. The agent's own review run leaves the
writer's comments.

## Reviews across machines

### A review lives with its workspace

A review lives on the reviewed workspace's machine, where the gates read it.
Its comments live there too. A space that reviews a workspace on another
machine has its comments opened there. They are numbered in the workspace, as
the user's are, and name the space as their opener, since the space's own
numbers belong to its own machine.

### The workspace's machine knows of a pending review

Only the workspace's machine knows whether the space has a pending review
there. So a thread the space opens towards that workspace without a kind asks
that machine first. Without a review, the thread is a question in the space's
own space. While that machine cannot be asked, the thread is refused unless it
names a kind.

### The writer's machine reaches every review

Only the machine that keeps a review knows whether the space wrote one there.
So the writer's completion, removal, their undo and its restore reach every
other machine as a request. The hub holds the request while a machine is
offline.

The writer's waiting comments on another machine keep it from cleanup, as its
threads here do. Its machine reads them from what that machine reports to the
hub.

To find the writer's only pending review, its machine asks every other machine
that runs a space it sees, and acts on the review there. A machine that cannot
be asked is left out, since nothing it keeps can be submitted or cancelled
until it returns.

## Review tools

A [review command](../reference/run.md#review-commands), such as tuicr, owns
its storage, its exports and its forge login. Its feedback reaches an agent
only when the user hands it over.

## The gate

### An agent decides routine requests

Most requests the user approves only check routine things. So a permission of
`"gate"`, between `"direct"` and `"propose"`, has an agent decide them first.
It is opt-in, since no built-in mode sets it.

The gate decides before a coordinator, since a coordinator's attention costs
as the user's does. A request the gate passes on takes the `"propose"` route.

### The gate saves attention

The gate is no reviewer. It checks conventions and mistakes, and what it saves
is the user's attention. So it decides even when it is unsure, and denies with
a reason that the agent can answer by asking again. It passes a request on only
for a case the user wrote down in `[gate] escalate`, and the daemon checks that
it quoted that case word for word.

### The gate adds no way to act

Allowing a request approves it through the path that a decision takes. So the
gate gives an agent nothing it could not do with the user's approval.

The gate reads its rules from the registered checkout, never from the
requesting worktree. So an agent cannot write the rules that judge it.

The gate may look further than its prompt with the `shoal` commands that only
read. It runs them under a scope of the requesting space that ends with the
run, and the daemon refuses that scope every change. That scope is
[cooperative](overview.md#scope-is-cooperative), as every scope is. The run's
sandbox opens Unix sockets, since neither tool can open only the daemon's on
Linux. So a run that sets its scope aside reaches only what the agent it judges
already reaches.

### The gate holds no slot

The gate's run holds no heavy slot, since agents wait on it.

### A session per space and subject

The gate keeps a session per space and subject. It thus keeps its
understanding and the prompt cache across retries, without mixing unrelated
requests. Every session opens with the same text, so a new one reuses that
cache.

A retry is a new request in its session. The third denial of one action stands
until the action changes, so an agent cannot wear the gate down.

### A failure denies

A failed run denies the request, since nothing is allowed without a verdict. It
notifies no one, since the agent can retry or ask. A gate that cannot be set
up, such as one without an agent to run, notifies the user too, since only the
config fixes it.

## The crate's boundary

Local reviews and the gate both decide in one-shot agent runs, so one crate
holds both. The `Reviews` service keeps the running reviews, their checkouts
and the gate's sessions. The agent queue admits each run through the crate's
interface, as a handle that the run holds until it ends.
