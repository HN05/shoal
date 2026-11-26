# Modes and guidance

This file covers modes, the guidance agents receive, the plan, coordinate and
setup modes, and the `shoal` skill.
[Modes and permissions](../reference/modes.md) says what each mode and rule
does, and the
[configuration reference](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#modes-and-rules)
how to configure them.

## Modes

### One command for every start

Every agent starts with `shoal start`, in a mode. A mode is a named entry. It
lists the rules the agent's system text carries and holds the instructions its
first message ends with. It says which kind of space the agent starts in, and
whether that space belongs to a repository. Only a chat ever belongs to none.
One command thus replaces a command per kind of start.

### The mode decides the kind of space

The mode alone decides the kind of space, and no flag overrides it. A mode's
rules and instructions thus always describe the space its agent is in.

With `--in`, the mode's kind gives the agent its role in an existing
workspace. A mode that starts in a workspace starts the workspace's own agent
and creates no space. A workspace thus keeps its place, name and links across
agents. A mode that starts in a chat starts a reader beside it in the
worktree. No other command starts a new agent in an existing workspace.

### The recorded mode

The space records the mode its agent started in, with the agent's name and
model. The record survives every restore and move, so the agent is told its
own mode's rules again.

### Choosing a mode

Without `--mode`, a start on a terminal asks for the mode, since the mode
decides what the agent is told. The picker lists the start's default first, so
Enter keeps it. Without a terminal, the start takes the default, so scripts and
agents need not name a mode.

The picker follows the checks of the given arguments, as every prompt does.
One function in the CLI settles every start, including those of `--in`,
`repo setup` and `contribute`. It refuses what the arguments alone refuse,
finds the repository and settings without asking, and checks the agent. It
then sends one `CheckLaunch` before its first picker.

The daemon settles that launch as it would start it, but records nothing. Once
the mode is decided, or the arguments name what only a workspace takes, it
checks the whole placement of the mode's kind. Otherwise it checks what every
kind shares, such as the repository, the parent and the given permissions.

The pickers then ask for what the launch leaves open, whatever the kind, such
as the repository or a new workspace's source. A start that creates a
workspace thus asks for its mode before its source.

### Mode labels

A mode may have a forge label, `[modes.NAME] label`. An issue that carries it
takes that mode next. For example, the plan mode's label marks an issue that
needs the owner's choices, and the worker's an issue whose body holds a plan
to implement without asking.

The label lives on the mode, so every mode, built in or the user's, joins the
flow the same way. Labels are configuration: Shoal assumes no label scheme and
ships none. Issues without a mode's label stay outside the flow.

The labels decide the mode of every launch on an issue or PR that names none.
That covers `shoal start` with or without a terminal, triggers and apps. The
label already says which work comes next, and an agent that starts work on an
issue means the same as the user. A script that wants a fixed mode names it.

An item with the labels of several modes has no right default, so a start
without a terminal refuses it.
[Without `--mode`](../reference/modes.md#without---mode) says how each start
chooses.

The daemon reads the labels, so every caller gets the same mode. `shoal start`
asks the daemon for that mode before it offers its picker.

## Guidance

[What an agent is told](../reference/modes.md#what-an-agent-is-told) lists the
layers an agent receives, in order.

### Shoal's guidance

The facts of the space and the space rules make up Shoal's guidance. It is
built in and precedes every agent's rules, in any space and any mode. Adding
rules thus never freezes or replaces it.

The guidance depends on the kind of space, never on the rules a mode lists.
A custom chat mode is thus told what a built-in one is.

The facts state only what is true of the space. They hold no practices.

The space rules hold the practices Shoal's own mechanics depend on, such as
loading the skill first. Every agent receives them, so they stay minimal and
vague. What a command does and when its conditions hold belong in the skill
and the user rules. A space rule thus has no conditions to keep in step with
them.

### Rules and instructions

Practices beyond the space rules belong in user rules, which a mode lists and a
user or repository may replace. Rules are named and shared, since several
modes need the same guidance. Instructions say how to do one mode's job.

The built-in modes list the `work` rule, one short sentence per practice of
Shoal's workflow. General work habits belong in a repository's own
instructions, not in Shoal's rules.

Shoal writes no copy of its built-in texts into the config directory. A copy
would freeze the default for whoever had it.

### System text and first message

Rules go through the agent CLI's system channel, so they hold for the whole
session, across compaction. A restore passes them again, since a resumed CLI
starts without the arguments that carried them.
[Instructions](agents.md#instructions) says how each tool receives them.

Instructions follow the subject once, in the first message. A restore thus
never repeats a task the session already has.

The subject, an issue or the prompt given, reads the same in every mode. Modes
thus differ only in what they add.

### Configured text

Modes and rules are named sections whose options layer one by one, as every
config option does. A repository's text follows the global text instead of
replacing it, so a repository extends a rule without restating it. An empty
repository value suppresses only the repository's addition.

Rendering substitutes known placeholders once and never evaluates what they
contain. It never renders the subject, which the forge or the user wrote.

## The plan mode

The plan mode refines an issue before anyone implements it. It settles the
plan in a chat, not a workspace. Planning changes no code, and it needs the
user's attention and their authority to rewrite the issue.

The plan goes into the issue body, because the body is all an implementing
agent's prompt carries.

The planning task is the plan mode's built-in instructions, which a repository
extends like any mode's text. It keeps the discussion in the conversation.
Threads and forge comments serve agents blocked during implementation.

The agent does not offer to start the implementation. When the user has it
start a workspace, the chat coordinates it.

A plan that splits the work into sub-issues opens them and hands the issue to
the coordinate mode, through the label of that mode where it has one.

## The coordinate mode

The coordinate mode carries out an issue whose plan splits it into
sub-issues. The user starts one agent and follows one chat, not each piece.

It is a mode of its own, not the master session's `orchestrate`, which manages
every space rather than one issue's work.

It starts a chat linked to the issue as `assigned`, since it changes no code.
Its agent never waits for the user to start, so its launches leave the
terminal alone.

Its agent drives the work with the commands every agent has, told how by the
mode's instructions. Shoal adds no daemon automation for it, so a repository
changes the coordination by changing the instructions.

The agent finds the sub-issues wherever the issue records them. Shoal
prescribes no format, since forges and authors record them differently.

It starts each sub-issue's agent with `shoal start --issue`. The agent is then
its child and reports to it, and the sub-issue's label decides its mode, as
for any start.

It orders the sub-issues with blockers. The forge's recorded blockers already
hold an agent. For a dependency only the text states, the agent adds an issue
as the blocker when the work needs the other merged, or the other's space when
it can build on that branch before it merges.

It starts the sub-issues whose plan is settled first. A sub-issue that needs a
plan waits until the work it builds on is done, since its plan then reads the
code as it is. Its plan chat opens in the background, so the agent alerts the
user once it runs. Once the plan is settled, the agent starts the
implementation.

It does none of the code work. It answers or passes on its children's threads
and approvals and follows their reviews, CI and merges. Once every sub-issue
is closed, it records what was done on the issue, closes it and finishes.

## The setup mode

`repo setup` starts one agent in the built-in `setup` mode, in a workspace on
the `shoal-setup` branch. `config setup` starts the same mode for the global
config, in a chat without a repository. That chat's reach lets it read how
every repository is set up.

The agent asks the user about each setting before it writes it, reading the
options from the configuration reference. Only the user knows which of the
settings a project could use they want.

The agent then asks where the repository config goes:

- committed, so it arrives as a reviewable commit that every clone shares;
- saved as the local config in the repository directory's `.shoal/`, for a
  repository that won't take the file or for one user's settings.

One agent serves both answers, since every workspace agent already works in
that repository directory. A local config applies as soon as it is saved. The
agent tests it in its own worktree, and its `done` removes the workspace and
its empty branch.

`setup` takes the rules and reach of a base mode and adds only its
instructions. Config thus cannot change what it does.

The user starts a setup to wait for its agent, so neither command waits in the
agent queue.

## The `shoal` skill

### Facts, not practices

One skill, `shoal`, says what Shoal's commands do, what they refuse and what
follows from them. Workspaces, chats and sessions outside Shoal read the same
skill.

The skill states no practices. Those are user rules, which a user can replace,
while an installed skill stays in step with the binary.

### Installation

[Agent skill](../reference/install.md#agent-skill) says how to install the
skill.

Shoal installs the skill at user scope, independent of the daemon, and never
from a scoped execution. Its availability registers nothing.

Skill directories describe the machine, so `[machine.agents.<name>]` names
them and a repository cannot.

Shoal installs only the skills built into its binary. Every installation, a
package manager's included, thus installs the version it runs.

### Skill files

Each installed `SKILL.md` is a regular copy, because Codex skips linked ones.

Each skill directory records what Shoal installed there. Shoal owns a skill
only while it matches that record, so it updates or removes only those.

Unscoped commands refresh the directories that still hold a Shoal skill, so an
upgrade needs no reinstall. The daemon does not, since it may run without the
user's tool environment.

Shoal keeps a skill the user changed or removed until an explicit install, as
a package manager keeps a modified configuration file.
[Skill updates](../reference/install.md#skill-updates) says what that install
does.
