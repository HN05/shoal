# Agent stops and restores

This file covers how agents stop, sleep, restore, restart and are interrupted,
and the recovery records that carry them.

Stop and exit alerts carry the pressure reason and recovery path, distinguishing
automatic restore from manual recovery and missing recovery records. Persist a
minimal agent recovery record with the stop reason before delivering the stop,
so a lost wrapper does not lose the handoff; failure to save warns without
disabling overload protection. Agent session recovery is automatic for built-in
Codex and Claude resume commands, or when explicitly configured for another
agent, and load and disk space have recovered, except that an agent stopped
between turns has nothing to do until an update arrives for it, so that update
restores it instead, and its restore holds no heavy slot until then. `shoal
resume` of an agent whose wrapper waits is the user asking for it, so it uses
the wrapper's queued restore, which then waits only for a heavy slot, rather
than refusing the agent as running or starting a second beside it. A restore,
manual or automatic, of an agent stopped mid-turn, or whose turn is unknown,
prompts it to continue its unfinished assignment, naming when and why it
stopped, which every stop that saves an agent records and a last session keeps,
and that its subagents and background commands ended. One that stopped or exited
on its own between turns is not told to continue: its session opens with only
the handoff it needs, or with the wake prompt whose hook delivers the updates
that wait for it, or with no prompt, and waits. Configured restore commands
receive the prompt through `{prompt}`, which an argument of exactly `{prompt}`
leaves out when there is none. Never replay the original task prompt. Keep
waiting wrappers tracked; they reattach like running commands, so a daemon
restart or crash hands the wait to the next daemon instead of cancelling the
restore. A waiting wrapper's session shows the agent asleep under what it last
showed, and Enter there starts its queued restore, as the user at the agent's
place asks for it; the wrapper asks over its own connection, which only that
execution's wrapper holds, so a scope token it inherited does not decide it.
`attach` to that session starts the restore too and attaches once the agent
runs, under every place recorded for it, and its pickers offer the workspace and
that session as stopped. `attach` wakes an agent whatever the limits and rank,
since the user asks for it: without a session showing an agent, it starts the
restore an asleep agent waits for, and without any session left, where only a
restore gives it something to show, it restores a stopped agent as `shoal resume
--now` does, through that restore, so it never starts a second execution beside
a waiting one. A daemon update waits for a stop and a restore waiting for load,
but not through a wait for an update, which may last indefinitely; that wrapper
reattaches to the updated daemon. Restores wait in the agent queue, and manual
recovery persists across restarts. Saved recovery represents unfinished work
until restored or explicitly forgotten, except that a done assignment needs no
agent, so a done workspace's agent is restored neither automatically nor by
`shoal resume --all`, only by resuming that workspace. Resuming it says the work
is not finished, so `shoal resume` withdraws the completion, whichever wrapper
restores the agent, and a protection stop restores it again; Shoal's own
restores and restarts keep it. The daemon owns resumes: a request names a space,
and that space's daemon decides and carries out the resume, continuing a turn an
interrupt ended with a message that wakes the agent, whose own wrapper types the
prompt in its terminal as for any message, so the daemon writes to no terminal,
queuing a failed launch again, or queuing the restores of what a stop saved,
started at once for `--now`, and answers with one outcome once it acted or
queued. The CLI keeps only what needs the caller's terminal, `--focus`, so no
queue entry or execution of another space crosses a resume, and a discard is one
request naming a space the same way. An idle heavy agent, a workspace's or a
chat's, sleeps once it has waited for the user: Shoal stops it as protection
stops an agent between turns, and an update restores it although its assignment
may be done, since Shoal rather than the agent ended it. Load plays no part, so
the recovery policy does not apply. What keeps an idle agent running is the user
working with it, not its prompt cache: a session's second request misses the
cache whichever process sends it, and every later one, the first after a resume
included, reads the whole history, so stopping and restoring an agent costs no
cache that keeping it would save. The user interacts with an agent by a key it
reads in its terminal, which its wrapper finds by the terminal's access time
(reading a key moves it, the agent's output does not), leaving out the keys
Shoal types itself, which Shoal reports before it types; by a prompt they
submit; and by `shoal attach`, following the agent's session, a message they
send it, a restore or restart they start, `shoal start --in` and Enter on the
sleep screen, which the daemon tells by the unscoped caller, whatever machine or
device it came from. Updates, Shoal's and a coordinator's messages, idle wakes
and what Shoal or a coordinator starts are not interaction, nor is a terminal
showing the agent, since a user may attach and walk away. The user is engaged
with an agent while they interacted since its turn before the last ended, so a
turn whose wait passes without them ends it, and later turns updates start wait
only the floor. An agent the user is not engaged with sleeps after a floor of
seconds, which lets hooks settle and a follow-up turn start; one they are
engaged with waits twice their reply time, from the later of the turn's end and
their last interaction, between the floor and `[sleep] max_wait_minutes`, which
an agent waits without a reply time. The reply time is the average, over the
space's last turns the user was engaged with, from a turn's end to their first
interaction after it, the first key rather than the prompt so that reading
counts, each counted at most the longest wait, so that one return after hours
does not decide the waits after it; a space without one takes the user's average
over every space. Timers the agent keeps itself would end with its stop, so
Claude Code's timer tools are refused by a hook that reads only the tool's name
and names `shoal sleep`'s timer, and the model, also in a `/loop` the user
types, uses the one Shoal keeps. A light agent uses too little of the machine to
be worth a restore, so it never sleeps for being idle, and `shoal sleep` only
ends its idle wakes. A restore resumes the session from its launch arguments, so
what the user changed inside it, such as its permission mode, approvals for the
session or its model, does not carry over, and Shoal does not carry it. An agent
Shoal stopped on its own and brings back on its own is asleep, and one that
waits for `shoal resume` is stopped, so the user tells an agent that comes back
from one that needs them. Every stop that queues a restore Shoal starts on its
own puts the agent to sleep: protection, idle and blocked stops, signal stops
and making room. `shoal stop`, an agent that quits on its own and the agent of a
removed or converted space are stopped, and a persistent stop stops the spaces
below first, while a parent may sleep before its children, since their reports
wake it. The asleep state is the restore's own: the restore keeps why the agent
sleeps and what wakes it, so lists, status and the inbox read it without the
recovery records, and `shoal attach`, the user asking for the agent, starts that
restore at once. `shoal sleep` is the agent saying it has nothing to do until
something arrives, so it ends the agent's idle wakes, and it takes the agent's
timer, since a timer says the same until a time: one per space, which a new one
replaces, kept as daemon state through stops, restores and restarts until it
fires as an alert, `--undo` cancels it or the space is done or removed. A loop
sets its next timer each turn, so Shoal parses no recurring schedule. Queued
work that needs a slot or memory may also put an idle agent to sleep before its
wait ends, or where `[sleep]` keeps idle agents running, lowest rank and
priority first and never above the entry's priority, but never one the user is
engaged with, since nothing overrides an engaged user, and never a light one,
which frees too little. An agent nothing will wake sleeps too, since the user's
attach or Enter on its sleep screen wakes it. Schedules only order the queue. A
woken agent reports no idle turn until it has worked one, so it never sleeps
again before it acts on what woke it. A space's reported turn state counts the
turns its agent starts, a count that continues for a restored agent or another
execution, and a protection stop is recorded by execution with the count it was
decided at, until the agent runs again or its restore is cancelled. A stop
chosen between turns records itself only while the agent is still idle in the
turn it was chosen in, and a report that starts a turn records none once a stop
is recorded, leaving its update for the restore: each decides in one
transaction, so whichever commits first wins, and the stop reaches the wrapper
only after its record commits. A load or disk stop the store cannot record, as
on a full disk, still stops the agent, as `shoal stop` does, leaving it for
`shoal resume`. A workspace with several agents keeps the turn of the one that
reported last, leaving the others' unknown, which counts as working. A record an
earlier daemon left authorizes no signal: a reattaching wrapper restores it or,
when its restore no longer waits, it is forgotten. A record names the stop's
stage: the daemon saves `stopping` before delivering a stop, and the wrapper
saves `stopped` after ending the agent. At startup, and whenever an execution
becomes unknown, the daemon forgets an unknown execution with a saved record
once its wrapper has exited and no owned process survives, if its launch
identity is complete or the record reached `stopped`, since the daemon clears a
stopped child's identity only after verifying the stop. Live wrappers are
rechecked when they exit; one that reattaches with its command running discards
a `stopping` record, whose stop never arrived. Survivors and incomplete evidence
keep the execution, are rechecked periodically so it clears once they exit, and
are the only outcome that notifies; `shoal stop` names them, and a resume still
reconciles the executions it restores. An unknown execution whose wrapper, and
child when recorded, ran in an earlier boot needs no evidence: restarting the
machine ended its processes, while its recorded process group may name
another's, so the daemon forgets it, keeping its record, or else the session its
agent reported as a last session. The daemon records each execution's agent name
for that. An unknown execution without a record or a recorded child or process
group never started its command, such as a setup whose wrapper left first; the
daemon forgets it once no process at its wrapper's PID has the recorded birth
and none of its processes survives, and rechecks a live wrapper when it exits.
Agent metadata is transient, so restart cannot select disconnected survivors.
`shoal restart` starts each agent in a new wrapper: the daemon sends the stop
protection sends, with the reason `restarted by shoal restart` and recovery
whatever the overload policy, and once the agent's processes are proven stopped,
cancels the wrapper's restore, which every release finishes on with the agent's
record kept. The record becomes no last session, and its end no exit alert; the
daemon then records the record's restore as admitted and starts it through the
queue start worker, as `shoal resume --now` does, in the terminal host of the
agent's recorded place. The new wrapper runs in a new process tree, so a restart
recovers an agent whose wrapper's tree broke, such as one that lost access to an
external volume. The record keeps the agent's slot until the admitted restore
takes it over, so the restart neither queues behind launches nor loses the slot
to them; a start that fails or leaves the queue before the agent registers fails
the restart, and a stop or completion that cancels the workspace's restores
cancels it under the queue gate the restart records its start under, leaving the
record for `shoal resume` either way. The restart, with the runs that asked for
it, follows the agent to the execution the new wrapper registers, which answers
it once the agent starts with the place that execution registered, so the run
can name where the agent runs now. `--in-place` restarts through the same
wrapper, which keeps the agent in its terminal: the daemon resumes the agent
once its processes are proven stopped, outside the agent queue. A chat's agent
restarts through the same path. The daemon owns restarts: a request names a
space, whose agent the daemon picks, and answers once the daemon restarted it,
recorded the restart for later or skipped it, naming why, such as a protection
stop that `shoal queue start` ends at once. Every restart is a store record of
its space, how it restarts, its permission change and who asked, so it outlives
the request and the daemon: the turn ends watcher that conversions use acts on
one that waits for the agent's turn to end or for its place in the machine's
line of sequential restarts, and a failure with no request left to answer
reaches the coordinator that asked or the user. Once the daemon begins it, at
once or when the watcher acts on it, the record stays until the agent's restore
applies its options, as the wrapper resumes the agent in place or a new wrapper
registers it, also while the restart waits for the stopped agent's processes:
each wrapper that reattaches to a later daemon meanwhile continues it with them,
and one that reattaches still running the agent, without the stop, lets the
restart begin again. Lists, status and `shoal restart --cancel` take only a
restart not begun yet. A newer request supersedes the earlier one as if that ran
to its end first: permission changes merge key by key, the newer winning, while
when and how come from the newer; a restart under way takes the newer request
until its wrapper settles how and with what permissions it starts the agent, and
after that the newer one is recorded and runs once the agent does. An agent
between its wrappers is listed as the execution it ran as, and one a new wrapper
runs with the executions it ran as. The restore command resolves the agent
again, so an updated agent binary runs. A wrapper whose binary an upgrade
replaced execs the installed one when the daemon resumes its agent in place,
keeping its process, terminal and daemon connection: the daemon sends nothing
until the restored child starts, so no control is lost. The resume names the
binary the daemon runs, and the wrapper continues only when the installed file
is that binary, whatever path either started from, since the daemon holds back
its own update until the agent starts; the installed release bounds the commands
it runs to prepare the agent. The installed release registers the execution
again on that connection, which records its place and returns a new plan, then
restores the agent, so new wrapper code reaches running sessions without
stopping them. The daemon restarts agents together by default; `--sequential`
spreads the load of starting them through its line, starting the next once the
restarted agent runs and at least the admission stagger after the previous
start. Agents without a restore command and executions whose wrapper is gone are
left to `shoal stop` and `shoal resume`; a restart that cannot start the agent
leaves its record for `shoal resume`, naming the space whose process asked for
it, which may then resume the agent as after its own `shoal stop`. `shoal stop
--interrupt` redirects an agent without stopping it: the agent's own interrupt
key cancels its turn and keeps its process, background shells and subagents,
which a stop and resume would end and start again. The CLI types the key through
the terminal host of the place the daemon recorded for the agent, a workspace's
or a chat's, so a client driving Shoal need not know each agent's key or each
host, and the daemon records the turn's end, which no hook reports. The daemon
keeps the interruption with the turn state the agent reported until a turn
starts or ends again; `shoal resume` continues the interrupted turn, as it
continues stopped work, so a client pairs one command with each.

Manual stop ends connected tracked executions through their wrappers, preserving
work and leases, and means the user wants its agents down until they resume
them: each stopped agent of the workspace whose execution ended, including one a
signal or protection stopped before, becomes a last session, as an agent that
exits on its own leaves one. The wrapper saves its recovery record as for any
stop and the daemon converts it once the stop finishes, so wrappers of earlier
releases stop the same way. Taking agents down briefly is `shoal restart`. A
launch waiting in the agent queue to start in the workspace has no session to
save, so a manual stop cancels it before stopping anything, under the queue gate
so admission cannot start it after, and reports it; one already starting belongs
to its agent. `shoal stop` of a waiting launch of a new workspace cancels it as
`shoal rm` does, and `--all` also selects the workspaces such a launch waits to
start in. `shoal repo move`, which starts the stopped work again after the move,
keeps them. User commands save their arguments, which a restore reports once, as
the restored agent's first prompt or else to the user, and never reruns, because
replaying an arbitrary command is not known to be safe. Scoped callers stop only
the executions of the spaces their scope reaches but their own.

An agent that exits on its own before its assignment is done leaves its
workspace a last session for `shoal resume` instead of stopped work: the user
may have quit it on purpose, so it takes no heavy slot, keeps nothing from
cleanup and comes back only when `shoal resume` names it or `--all` queues it. A
workspace keeps one per agent, so resuming several stopped together restores
each; a new agent replaces them, since the user moved on, while a restored one
consumes only its own, which the wrapper says when it registers. Built-in agents
resume the session ID their hooks report, since the workspace's latest session
may belong to another agent. A restored agent carries the session of the record
it restores, with how its space changed, such as a conversion, until its hooks
report one, so a restore that exits or stops before then leaves that session to
resume, not one without it.
