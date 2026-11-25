# Modes and guidance

This file covers modes, the guidance agents receive, the plan and setup modes,
and the `shoal` skill.

Refining an issue before implementation is the plan mode's, which settles it
with the user in a chat. `repo setup` likewise starts one agent, in the built-in
`setup` mode in a workspace on the `shoal-setup` branch. A setup agent asks the
user about each setting before it writes it, reading the options from the
configuration reference, since only the user knows which of the settings a
project could use they want; it then asks whether to commit the repository
config, so it arrives as a reviewable commit that every clone shares, or, for a
repository that won't take that file or settings for one user, to save it as the
local config in the repository directory's `.shoal/`. Every workspace agent
already works in that repository directory, so one agent serves both answers; a
local result applies as soon as it is saved, the agent tests it in its own
worktree and its `done` removes the workspace and its empty branch. `setup`
takes the rules and reach of a base mode and adds only its instructions, so
config cannot change what it does. `config setup` starts the same mode for the
global config in a chat without a repository, whose reach lets it read how every
repository is set up. The user starts either to wait for its agent, so neither
waits in the agent queue.

A mode may have a forge label, `[modes.NAME] label`, which says that an issue
carrying it takes that mode next, such as the plan mode for one that needs the
owner's choices and the worker for one whose body holds a plan an agent
implements without asking. The label lives on the mode, so any mode, built in
or the user's, joins the flow the same way; labels are configuration, so Shoal
assumes no label scheme, ships none, and refuses two modes with the same
label, and issues with no mode's label stay outside the flow.
The labels decide the mode of every launch on an issue or PR that names
none, a `shoal start` without `--mode`, with or without a terminal, a
trigger's or an app's, since the label already says which work comes next and an
agent starting work on an issue means the same as the user; a script that
wants a fixed mode names it. An item with no mode's label gets the
picker on a terminal and the default mode without one; one with the labels of
several gets the picker on a terminal and is refused without one, naming them,
since no default is right. A bare `--issue` or `--pr` picks the item first,
among those carrying the label of the mode `--mode` names where it has one,
so its labels can decide the mode when none is named; a named mode alone
never opens that picker. The daemon reads the labels, so every caller gets
the same mode, and `shoal start` asks it for that mode before it offers its
picker.
The plan mode settles a plan in a chat, not a workspace: planning changes no
code, needs the user's attention and their authority to rewrite the issue, and
the chat coordinates the implementing workspace when the user has it start
one; the agent does not offer to. The plan goes
into the issue body because the body is all an implementing agent's prompt
carries. The planning task is the plan mode's built-in instructions, which a
repository extends like any mode's text, and it
keeps the discussion in the conversation: threads and forge comments serve
agents blocked during implementation.

One skill, `shoal`, describes what Shoal's commands do, what they refuse and
what follows from them, for workspaces, chats and sessions outside Shoal alike,
and states no practices: those are user rules, which a user can replace, while
an installed skill is kept in step with the binary. It is installed at user
scope, for every tool whose skill directory already exists unless a tool is
named, independent of the daemon and never from a scoped execution; its
availability registers nothing. Each skill directory records what Shoal installed there;
Shoal owns a skill only while it matches that record, so it updates or removes
only those, and treats files from before the record as its own and an
unrecorded file beside a record as the user's. Unscoped
commands other than the daemon refresh directories that still hold a Shoal
skill, so upgrades need no reinstall; the daemon may not share the user's tool
environment. Debug builds are development builds that share the user's home
with the installed Shoal, so unless tests in a temporary home set
`SHOAL_DEV_USER_FILES=1`, they change no user-level files outside their state
directory, skills included, and refuse the user's own state directory, whose
daemon serves every running agent, however it is selected, and writes of the
global config and the files beside it in the user's own config directory, from
which running agents read their settings; unit tests refuse that directory
whatever they inherit; the password
database names the user's home, so a temporary `HOME` does not. A changed or removed skill stays as the user left it until an
explicit install, which restores removed skills, asks about changed ones in a
terminal like a package manager's modified configuration files, and with
`--force` replaces them. `[machine.agents.<name>]` settings name skill directories, with defaults for built-in providers. Skill directories describe
the machine and cannot be set per repository. Shoal installs only the skills
built into its binary, so every installation, a package manager's included,
installs the version it runs. Installed `SKILL.md` files are regular copies, because Codex
skips linked ones. Homebrew launches the binary directly, without a shell.

Every agent starts with `shoal start`, in a mode: a named entry that lists the
rules its system text carries, holds the instructions its first message ends
with, and says which kind of space it starts in and whether the space belongs to
a repository, only a chat ever belonging to none, so one command replaces a
command per kind of start. Without `--mode`, a start on a terminal picks the
mode, since it decides what the agent is told, and lists the start's default
first so Enter keeps it; without a terminal it takes the default, so scripts and
agents start as before. The picker follows the checks of the arguments given, as
every prompt does: one function in the CLI settles every start, `--in`'s, `repo
setup`'s and `contribute`'s included, refusing what the arguments alone refuse,
finding the repository and settings without asking and checking the agent, then
sends one `CheckLaunch` before its first picker. The daemon settles that launch
as it would start it, short of recording anything: the whole placement of the
mode's kind once the mode is decided or the arguments name what only a workspace
takes, and otherwise what every kind shares, the repository, parent, given
permissions and named blockers. The pickers then follow what the launch leaves
open, not a kind: the mode, the repository where the mode starts in one, a new
workspace's source, a plan's issue and the agent, so a start that creates a
workspace asks its mode before its source. The mode alone decides the kind of
space, with no flag overriding it, so a mode's rules and instructions always
describe the space its agent is in. With `--in`, the mode's kind gives the agent
its role in that existing workspace: a mode that starts in a workspace starts
the workspace's own agent, creating no space, so a workspace keeps its place,
name and links across agents, and a chat-kind mode starts a reader beside it in
the worktree; there is no other way to start a new agent in an existing
workspace. The space records the mode its agent started in, carried with the
agent's name and model through every restore, so a restored or moved agent is
told its own mode's rules again. Rules are named and shared, since several modes
need the same guidance, while instructions say how to do one mode's job. Rules
go through the agent CLI's system channel so they hold for the whole session,
across compaction, and a restore passes them again, since a resumed CLI starts
without the arguments that carried them; instructions follow the subject once,
in the first message, so a restore never repeats a task the session already has.
The subject, an issue or the prompt given, is worded the same in every mode, so
modes differ only in what they add. Modes and rules are named sections whose
options layer one by one; their texts select repository additions by
local/worktree precedence and append them to the global text. Empty repository
values suppress only the addition. Repository Markdown files, at the root or in
`.shoal/` like the repository config and refused in both, and global files
beside `config.toml` sit below inline TOML values at each level; provenance
names the highest configured layer and the file it read, and reports the
combined text. What an agent is told comes in layers, in the order it receives
them: the skill, which says how each command works and which a space rule has it
load first; the facts of its space; the space rules; the user rules its mode
chains; and the task, its subject and mode's instructions in the first message.
The facts and space rules make up Shoal's guidance, which is built in and
precedes every agent's rules, in a workspace or a chat and in any mode, so
adding rules never freezes or replaces it. They depend on the kind of space, a
workspace, a chat in the checkout or a chat in a workspace's worktree, never on
the rules a mode lists, so a custom chat mode is told what a built-in one is.
The facts state only what is true of the space and hold no practices. The space
rules are the practices Shoal's own mechanics depend on, sent to every agent and
kept minimal and vague: what a command does and when its conditions hold belong
in the skill and the user rules, so a space rule never has conditions to keep in
step with them. Practices beyond these belong in user rules, which a mode chains
and a user or repository may replace. The built-in modes list the `work` rule,
one short sentence per practice of Shoal's workflow. General work habits belong
in a repository's own instructions, not in Shoal's rules. Nothing is seeded into
the config directory: a seeded copy froze the default for whoever had it.
Rendering substitutes known fields once without evaluating their contents, and
never renders the subject, which the forge or the user wrote. The former prompt
templates are refused with what replaces them rather than loading with a
warning: the format broke once, so no setting keeps two names.
