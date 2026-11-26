# Reviews

This file covers `shoal-reviews`: local reviews, reviews a space writes, the
gate agent and advisory repository reviews.

Advisory repository reviews default to Codex, retaining configured agent overrides
and automatic fallback. Repository code audits are manual read-only inspections per focus over the
entire application and open one issue per new finding. Failed or incomplete attempts
open no issues; audit results are advisory and do not gate merges.

`review run` chooses between the `review` command and a local review, which
follows the review bot. The daemon runs the reviewer once, read-only in a
detached checkout of the committed HEAD, so the review judges what the agent
committed and never sees or changes its uncommitted work. The reviews of a
workspace, or of a PR, continue one reviewer session, so a later review keeps
what the reviewer understood and reuses its cached prompt; an AI tool finds a
saved session by the directory it ran in, so the checkout has one path per
workspace and PR. The session adds context and decides nothing: each review is
given its instructions and history in full, and a change of agent starts a new
session. The reviewer writes its review with the commands everyone's reviews
use, under a scope that ends with its run and writes only that review: `thread
open` opens each finding on lines of its checkout, which resolve to the reviewed
commit, `send` in a finding resolved `fixed` reopens it when the problem
remains, and `review submit` finishes it with its summary and the written
reviews' verdict rule. Each finding is a `review` thread rather than a record of
its own, opened by Shoal and held by the reviewed workspace, so its holder
resolves it, its posts wait for the reviewer's answer and false positives name
it; the review record keeps its target, commit, reviewer, verdict and state. A
finding then reads, resolves, follows the code and shows in editors like every
other thread about code, and its discussion is posts. Findings hold their posts
until the submit, which delivers the summary and all its threads in one alert,
so a review wakes the agent once, and a reopen waits for it too, so the holder
sees none of a run that never submits. A run that exits without submitting fails
the review, which keeps the typed reasons failures name before it, and what the
run wrote is dropped before the review fails, its fallback runs or it runs
again. The holder resolves a thread Shoal opened, with a typed outcome carrying
its reason or issue; `fixed` points at the changes attributed to the thread,
which the next review receives with it, reopening it with a post saying why when
the problem remains, instead of rereading free text. The gates count the threads
a review opened or reopened, since a reopened finding is one that review raised
again. A holder's post in a finding's thread, instead of a resolution, resumes
the reviewer's session for that workspace or PR as a one-shot, read-only run in
the subject's place, which answers in the thread with `send` under a scope that
ends with its run and posts only there, for Shoal, so the agent can argue a
finding with the reviewer that made it; a run that posts nothing fails as a
review's run does, and Shoal posts why. Whether a thread waits for an answer is
read from its posts, so a pending one outlives a daemon restart. A finding
resolved `false-positive` outlives its workspace as a claim of its repository,
which every later review receives, so a wrong claim the reviewer would raise in
every change touching the same code is answered once. It is kept in a file
beside the repository's local config rather than a tracked one: the outcome
records it, with no command or commit of its own, reopening the finding's thread
withdraws it, and the user edits the file to drop or add a claim. The history a
review receives is context, not instructions. Failures are typed reasons
classified like the bot's, so backend output never reaches a message. The
30-minute limit is fixed: a review that long has stopped making progress. The
daemon starts the reviewer as a one-shot agent run, which any feature can use:
an AI tool's command without a terminal, which may resume a session an earlier
run reported, in its own process group, killed on timeout, cancel or a daemon
stop and found again by its marker after a crash. A one-shot run waits for
admission at the rank of the workspace it serves, ahead of that workspace's
agent, which waits on it. Protection stops a one-shot run before any agent,
since it has no running session to lose; it asks for admission again and waits
for healthy readings, as a stopped agent's restore does. A review the machine's
announced sleep would cut off stops the same way as the machine goes to sleep
and asks for admission again, which the asleep queue gives once the machine
wakes, without waiting for readings, and a daemon that starts again queues each
review that was running. A workspace's package carries its queued or running
review among its records, so the machine that continues it after a move, a
hand-over or a takeover queues the review again; the machine it left removes its
copy, review and all. A chat has no worktree or branch, so it reviews no changes
of its own: it names the workspace it reviews, or reviews its one linked PR as a
PR target, and its reviews are those of its linked PRs. A PR target, named by
URL or `--pr`, resolves a same-repository PR through the daemon's forge reads,
as the agent account for a scoped caller and the user's logins otherwise, and
reviews its head branch against the PR base, reusing the owning workspace after
fast-forwarding it to the pushed head; local commits ahead stay, divergence is
refused. A fork's PR opens its workspace at a commit without setup, since the
reviewer only reads the checkout. The daemon fast-forwards under a lifecycle reservation and the
repository's Git gate, after verifying the recorded worktree and its branch, so
a switched or replaced checkout is never moved. The user, or a parent agent
within its scope, reviews a space's work the same way, so what the agent
receives and what the gates count does not depend on who reviewed. Such a review
is a review record naming its writer, which writes it pending: the threads its
writer opens towards the workspace without a kind join it as `review` threads
that hold their posts, numbered so editors show them, and submitting delivers
them with the summary in one alert, as a local review's findings arrive,
recording the workspace's HEAD then; a comment keeps its own file version, so it
may point at unsaved lines. Joining by the pending review rather than a `--kind`
lets an editor's first comment start the review and a submit action send it. An
explicit kind opens an ordinary thread outside it. Its writer, the opener,
resolves each comment after checking the changes attributed to it, since only
threads Shoal opened are its holder's to settle, and Shoal's reviewer neither
reads, reopens nor learns false positives from them. A submitted review covers
its commit once its comments are resolved, and one without comments only when it
approves, so a summary alone neither opens a gate nor closes one a covering
review opened. Any review of the commit whose threads are unresolved decides the
gate over the others, since a later review settles none of them: an approval
leaves the reviewer's findings to be resolved, and the agent's own review run
leaves its writer's comments. A review lives on the reviewed workspace's
machine, where the gates read it, and so do its comments. A space reviewing a
workspace on another machine has its comments opened there, numbered in the
workspace as the user's are and naming the space as their opener, since its own
space's numbers belong to its own machine. Only that machine knows whether the
space has a pending review there, so a thread the space opens towards that
workspace without a kind asks it first: without a review the thread is a
question in the space's own space, and while that machine cannot be asked it is
refused unless it names a kind. The reviewed workspace's removal, or its
writer's, drops a pending review, whose comments a restore could never submit. A
writer's waiting comments on another machine keep it from cleanup as its threads
here do, read from what that machine reports to the hub, and its completion,
removal, their undo and its restore reach every other machine as a request the
hub holds while one is offline, since only the machine that keeps a review knows
whether the space wrote one there. Its machine asks every other machine that
runs a space it sees for its pending reviews there, so its only one is found
wherever it is kept and acted on there; a machine that cannot be asked is left
out, since nothing it keeps can be submitted or cancelled until it returns.
Forge reviews stay on the forge.

A `review` command such as tuicr owns its own storage, exports and forge
authentication, with explicit feedback handoff to agents.

Most requests the user approves only check routine things, so a permission of
`"gate"`, between `"direct"` and `"propose"`, has an agent decide them first,
opt-in since no built-in mode sets it. The gate is not a reviewer: it checks
conventions and mistakes, and the user's attention is what it saves, so it
decides even when unsure, denying with a reason the agent can answer by asking
again, and passes a request on only for a case the user wrote down in `[gate]
escalate`, which the daemon checks it quoted word for word. It decides before a
coordinator, since a coordinator's attention costs as the user's does; an
escalation then takes the `"propose"` route. Allowing approves through the path
a decision takes, so the gate adds no way to act. It reads its rules from the
registered checkout, never the requesting worktree, so an agent cannot write the
rules that judge it, and runs as a one-shot run that holds no heavy slot, since
agents wait on it. It may look further than its prompt with the `shoal` commands
that only read, under a scope of the requesting space that the daemon refuses
every change and that ends with the run. That scope is cooperative like any
other: its sandbox opens Unix sockets, since neither tool can open only the
daemon's on Linux, so a run that sets its scope aside reaches what the agent it
judges already reaches. A session per space and subject keeps its understanding
and the prompt cache across retries without mixing unrelated requests, and every
session opens with the same text so a new one reuses that cache. A retry is a
new request in that session, and the third denial of one action stands until the
action changes, so an agent cannot wear the gate down. A failure denies, since
nothing is allowed without a verdict, and notifies no one: the agent can retry
or ask. A gate that cannot be set up, as without an agent to run, notifies the
user too, since only the config fixes it.

## The crate's boundary

Local reviews and the gate agent both decide in one-shot agent runs, so one
crate holds both. The `Reviews` service keeps the running reviews, their
checkouts and the gate's sessions. The agent queue admits each run through the
crate's interface, as a handle that the run holds until it ends.
