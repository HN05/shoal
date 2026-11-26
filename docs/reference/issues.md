# Issues and pull requests

This file says what the `issue`, `pr` and `forge` commands do and which forges
Shoal supports.

## Forges

Shoal reads and changes the issues and PRs of the forge serving a repository's
`origin`. `github.com` is GitHub, `gitlab.com` GitLab and `bitbucket.org`
Bitbucket Cloud. The daemon asks any other host once which API it serves, through
GitHub's `meta`, GitLab's `version` and Forgejo's or Gitea's `version` on that
host, with the host's token below, and records the answer. The daemon asks recorded hosts
again when it starts or reloads its config, and records an alert when one now
answers as another forge. A host that answers as none of them fails until you
set its [`type`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#forges), which also stops Shoal asking it.

Shoal sends every request to the forge's API with curl, signed in with the
host's [`user_token`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#forges): GitHub.com at `api.github.com`, a
GitHub Enterprise Server under `/api/v3`, GitLab under `/api/v4`, Forgejo and
Gitea under `/api/v1`, and Bitbucket Cloud at `api.bitbucket.org`. It runs no
forge CLI. Changes Shoal makes
for a workspace use the host's `agent_token`, as [agent accounts](agents.md#agent-accounts)
describe. A token from `{ env }` read by the daemon, such as for PR watches, comes
from the daemon's own environment, which a service does not take from your
shell, so set the variable where the daemon runs or use `{ cmd }`. A token
command that fails or prints nothing fails the request. An issue or PR number may be written `#42`.

Forges differ in what Shoal can do there. `shoal forge capabilities` lists each
capability of the workspace repository's forge, such as resolving review
threads, rerunning CI jobs, labels or a merge method, and whether the forge has
it, asking the server where releases differ, with what to do instead when it
does not; `--json` returns `forge` and `capabilities`, each with `capability`,
`supported` and an optional `hint`. Processes inside a workspace and a chat's
agent read capabilities and reactions as the [agent account](agents.md#agent-accounts),
anyone else with your own logins.
A command that needs only a repository, such as this one, reads the repository
of the workspace or registered checkout it runs in, such as a chat's, else a
registered repository the user picks, and with `--repo`, named as for
`issue open --repo`, another repository, which needs none of them when its
URL names it. A
command that needs a capability the forge lacks fails before changing anything,
naming the forge and the capability.

`shoal forge reactions` lists the reactions the forge allows, under the names
`shoal view` reports: on Forgejo and Gitea those the instance's settings allow,
on GitHub its eight, and on GitLab the same eight as suggestions, since any emoji
it knows works. `--json` returns `names` and `accepts_other_names`. Bitbucket
has no reactions.

GitLab projects may sit in subgroups, its merge requests are PRs, and its item
URLs use `/-/issues/` and `/-/merge_requests/`. Reviewers are usernames, and
a merge uses the project's merge method, squashed with `--method squash`. CI
checks are the latest status of each job and external check on the head
commit, and reviews are reviewers' states and approvals; approval rules and
merge trains are not modeled.

Bitbucket repositories live under a workspace and its PR URLs use
`/pull-requests/`. It reports no merge conflicts. Reviewers are Atlassian
account IDs, declining a PR closes it, and `--method rebase` rebases and
fast-forwards. CI checks are the builds reported on the PR, and reviews are
participants' approvals and change requests.

### Rate limits and retries

A forge limits an account when it refuses a request for its rate limit, with
`429`, or `403` with `Retry-After` or no requests remaining. It also does when
it reports no requests remaining. Shoal then sends nothing to that forge as
that account until its `Retry-After` or reset time, at most an hour away.
Meanwhile [watches](links.md#failed-lookups) wait instead of reporting lookup
failures, and the daemon records a `forge_limited` alert.

The daemon repeats a REST read as a conditional request. It sends the `ETag`
and `Last-Modified` of the last response it got for that URL as the same
account, and takes `304 Not Modified` to mean that response still holds. It
keeps these in memory only.

The daemon sends every forge API read up to three more times, waiting longer
each time, while:

- its connection fails or breaks,
- it times out, or
- the forge answers with a 5xx other than a rate limit.

## Issue and PR actions

```sh
shoal pr open --label area/cli      # push, then link the branch's open PR or open one
shoal pr edit --title "Fix login" --add-label bug --draft
shoal pr comment --body-file notes.md
shoal pr comment --body-file notes.md --attach shot.png --attach 'demo.mp4#Login flow'
shoal pr comment --edit 812 --body-file fixed.md  # replace the body of comment 812
shoal pr reply 812 --body-file why.md   # in the thread of review comment 812
shoal pr resolve 812 --body-file why.md # reply, then resolve; unresolve reopens
shoal pr review 42 --comment --body-file review.md --line src/a.rs:7 'Why?'
shoal pr review 42 --request-changes --body-file why.md --comments-file lines.json
shoal pr close 12                   # reopen works the same way
shoal pr reviewers 12               # users and teams who can review PR 12
shoal pr update --method rebase    # take in the base's commits on the forge
shoal merge 12 -X rebase --delete-branch  # merge PR 12 on the forge
shoal merge pr --auto               # merge the linked PR once checks pass; --cancel-auto withdraws it
shoal pr revert 42                  # open a PR that reverts merged PR 42
shoal pr comment 12 --repo shoal --body-file notes.md  # in a registered repository, without a workspace
shoal issue ls --label bug flaky  # search by text; --state, --author and --limit too
shoal pr ls --state all --author dev
shoal issue ls --repo team/review-bot missed  # search another repository
shoal issue edit --add-label bug    # comment, close and reopen work too
shoal issue edit 12 --add-assignee kim --milestone v2  # --clear-milestone removes it
shoal issue open --title "Flaky watch test" --label bug
shoal issue open --repo team/review-bot --title "Missed finding" # in another repository
shoal repo labels                        # the repository's labels with their descriptions
shoal repo labels --repo team/review-bot # another repository's; forge capabilities and reactions too
shoal forge capabilities                  # what the repository's forge supports
shoal forge reactions                     # the reactions the repository's forge allows
```

`shoal pr open` verifies the workspace's worktree identity and that it is on its
recorded branch, then pushes the branch without forcing to its push remote
(`branch.<name>.pushRemote`, else `remote.pushDefault`, else `origin`), setting
its upstream, as a tracked command with your Git configuration and hooks and
its output on stderr. The daemon then links the open PR whose head is that
branch in the repository the push remote names, or opens one in the `origin`
repository and links it. A push remote on the same forge host other than
`origin` is a fork the PR comes from; GitLab merge requests from a fork are
refused. A new PR's title is
`--title`, else the first assigned issue's title, else the last commit's
subject; its description is the supplied `--body` or `--body-file`, or else a
`Closes #<number>` line for each assigned issue; its base is `--base`, else the base
workspace's branch, else the default branch. It is a draft with `--draft` or
[`[pr] draft = true`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#prs), which `--no-draft` overrides. Draft state, repeated
`--label`, repeated `--reviewer`, repeated `--assignee` and `--milestone TITLE`
apply only to a new PR. Its JSON adds
`created` (false for a PR it found), `linked`, and the `warnings` it prints on
stderr: [convention](#repository-conventions) warnings, and a failure to label,
request reviews, assign, set the milestone, attach files or read a new PR once
the forge opened it, which still records and links the PR with the labels and
details the forge reports it has, or as posted when it cannot be read.

`shoal pr` and `shoal issue` change an item of the repository of the space they
run in, a workspace or chat, or the one `-s`/`--space` names: the number or URL given,
otherwise the space's only assigned issue or PR, which needs its number or URL
when several are assigned. A chat acts in its
repository's registered checkout, and an issue it opens is recorded as opened
there, or with `--link` assigned to it. Outside a space they act on the
repository of the registered checkout they run in, as its agent account, and take
only a number or URL. `-r`/`--repo`, instead of `--space`, names any repository
as for `issue open --repo`, and an issue or PR URL names its own, registered or
not, to act on the same way from anywhere, so a client acting as the user needs
no workspace; a registered one is acted on through its checkout. Anyone reads
another repository's items, while changing them is yours, with your own logins:
a scoped process's change there is refused. Workspace processes act only from
their own workspace, `pr open` needs one,
and `pr revert` needs a space; a chat's revert that the forge cannot open itself
comes from a new workspace the chat opens.
Outside all of them, a non-interactive call fails saying so. `edit`
takes `--title`, `--body` or `--body-file`, and repeated `--add-label` and
`--remove-label`; PRs also take `--base`, repeated `--add-reviewer` and
`--remove-reviewer`, which take a team as `owner/team` on GitHub and Forgejo,
and `--draft` or `--no-draft`. Both kinds take repeated `--add-assignee` and
`--remove-assignee`, and `--milestone TITLE` or `--clear-milestone`. Edits apply in that order,
and a failed request stops the rest with the forge's message. Labels are
replaced as a set by name, an added exclusive label replacing the item's
label of its scope, and an edit whose added labels the item then lacks,
or whose removed labels it still has, fails naming them and the item's labels,
and those added that the repository has no label for;
Forgejo's draft state is the title's `WIP: ` prefix,
and GitLab's its `Draft: ` prefix. Assignees are replaced as a set by login,
and a milestone is named by its exact title, open or closed, including a GitLab
group's; an edit whose assignees or milestone the item then does not show, such
as a GitHub user who cannot be assigned or a second assignee on GitLab's free
tier, fails naming them and what the item has. Bitbucket issues take one
assignee, by account ID, and Bitbucket PRs take neither assignees nor milestones,
which [`shoal forge capabilities`](#forges) reports.
`shoal pr reviewers` lists the users and teams a PR can ask for a review,
except its author, by the `name` that `--add-reviewer` takes, with the forge's
`display_name` and whether it is a `team`. Users are GitHub's assignable
users, Forgejo's review candidates, GitLab's project members and the members
of a Bitbucket workspace, named by account ID; teams are those with access to
an organization's repository on GitHub and Forgejo. Processes inside a
workspace and a chat's agent read them as the [agent account](agents.md#agent-accounts),
anyone else with your own logins.
`open`, `edit` and `comment` also take repeated `--attach FILE` or
`--attach 'FILE#ALT TEXT'`: a non-empty file of at most 10 MiB for an image and
100 MiB otherwise, checked before anything is sent; the forge decides which types
it accepts. Each file is uploaded through the forge and replaces the body's image
references to it, such as `![The error](shot.png)` relative to the body file's
directory or the working directory, or otherwise follows the body: a PNG, JPEG,
GIF, WebP or SVG image or an MP4, MOV or WebM video is shown, any other file
linked with the alt text or its name;
`edit` without a new description adds them after the current one. GitHub and
GitLab keep uploads with the repository. Forgejo and Gitea keep them with the
item or comment, so a new comment or item is first posted naming the files,
then updated to show them; Shoal first refuses a file the instance's attachment
settings would refuse, by type or size. It resends an upload up to three times
while the connection fails before sending it or the forge answers 503, and a
comment's update also after a broken connection or another 5xx; a 429 is the
forge's [rate limit](#rate-limits-and-retries), which holds back requests until it ends.
GitHub needs write access to the repository and Forgejo
a token.
`reply`, `resolve` and `unresolve` take the ID of any comment in a review
thread, as `shoal view` shows it, before the PR's number or URL. `reply` takes
`--body` or `--body-file`; `resolve` posts one as a reply first when given.
Resolving a thread that has no reply asks for confirmation; `--yes` skips it.
`react REACTION` adds a reaction to the item, or with `--comment ID` to one of
its comments or inline comments as `shoal view` shows it, and `--remove`
removes it; the reaction is GitHub's name, as `shoal view` shows it, or one the
forge has, such as a GitLab emoji. A comment must belong to the item, which is
checked with the item's kind before anything is sent. A reaction already as
asked is left unchanged. Bitbucket has no reactions.
`comment --edit ID` replaces the body of a comment, or on a PR an inline
comment, as `shoal view` shows its ID, with `--body` or `--body-file`; its
`--attach` files are uploaded and shown as a new comment's are, after the
current body when no new one is given. The comment must belong to the item and
be written by the login Shoal acts as, which is checked before anything is
sent; a comment that already has the new body is left unchanged. Watches do not report a workspace
process's edit back.
`update` brings an open PR's branch up to date with its base on the forge,
`--method merge` (the default) by merging the base in and `--method rebase` by
rebasing onto it; workspace processes may update, and then pull the branch
before pushing again. GitHub refuses the update when the head moved since Shoal
read it, GitLab only rebases and does so after answering, and Bitbucket cannot
update a branch.
`shoal merge` merges a PR with `-X`/`--method merge|rebase|squash`, without
it by [`[merge] method`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#merges)
of the repository it acts in, or of the global config for another repository,
and without that by the repository's default: Forgejo's and Gitea's default merge
style, GitLab's merge method and squash setting, Bitbucket's default strategy,
or on GitHub, which has none, the first of merge, squash and rebase that the
repository allows. `--message` gives a squash merge its commit message, the
first line as its subject where the forge takes the subject apart, and is
refused for a merge by another method or by a default the forge does not
tell. `--delete-branch` deletes the
PR's branch on the forge when it is in the repository, never a local branch.
`--auto` has the forge merge the PR that way once its required checks and
approvals pass, merging now when they already do: GitHub's auto-merge,
GitLab's merge when the pipeline succeeds, or Forgejo's merge when checks
succeed. GitHub deletes the branch of an automatic merge only as the
repository's settings say, so it refuses `--delete-branch` with `--auto`, and
Bitbucket cannot merge later. `--cancel-auto` withdraws the automatic merge.
A merge and an automatic merge name the head commit Shoal read for them, and
the forge refuses one whose head moved by then, except on Bitbucket, which
takes none. Of the automatic merges, only GitLab's stays bound to that head
until it merges, dropping the merge when the branch moves; the others merge
the head the PR has once checks pass, as `shoal forge capabilities` says.
When the last check of a workspace that links the PR found files
[changed on both sides](branches.md#files-changed-on-both-sides) at the PR's head with its base branch,
a merge and `--auto` name them and ask before merging, such as
`Sensitive files changed on both sides by PR #12 and main: src/version.rs`;
`--yes` merges without asking, and without a terminal they refuse and name it.
They read only that record, so a PR without one, a record of another head or
base branch, or a PR no workspace links merges without a question. A merge is
refused when its warning changed while the question was open, including a
warning that appeared where none was. A scoped process merges, sets or
withdraws an automatic merge as the agent account,
as its space's [`merge` permission](modes.md#mode-permissions) allows: the request binds the PR
at its head commit, which the merge names to the forge, and the method, `-X` or
`[merge] method`, and a squash's message, so with `"propose"` a later commit,
another method or another message needs another approval. A merge that its [review gate](reviews.md#review-gates)
refuses goes no further.
[`shoal branch delete`](branches.md#branches-on-the-forge) deletes a PR's branch on the
forge once it has merged or closed.
`revert` opens a PR that undoes a merged PR into its base branch and refuses
one that has not merged. It takes `pr open`'s options except `--base`; its
title defaults to `Revert "<title>"`, its description to `Reverts #<number>`,
and its labels to the reverted PR's. Where the forge reverts PRs (the
`revert_prs` capability, GitHub only), the forge opens it from a branch it
creates, and the workspace records it as opened without linking it; a
workspace process cannot have it opened where its `pr_open` permission asks
for approval, since the forge's revert cannot wait for it. Elsewhere
Shoal creates a workspace on branch `revert-<number>` from the base branch as
`origin` has it, set up as any new workspace and a child of the workspace a
workspace process runs in. As that workspace's tracked commands, it reverts
the merge in one commit, after establishing which commits it added: a merge
commit whose later parent has the PR head's tree, against its first parent;
the base's latest first-parent commits when they carry the patches of the
PR's commits, as after a rebase or fast-forward; or one squashed commit whose
tree is the PR's head merged onto its parent. It fetches the PR's commits
from its head ref on `origin` once its branch is gone. A merge it cannot
establish, and changes setup left to tracked files, refuse the revert. It
then pushes and opens the PR as `pr open` does, linked
to the new workspace, and marks that workspace done with cleanup, so it goes
once the revert merges. A revert that conflicts, or that changes nothing,
removes the new workspace and fails, listing the conflicting files; when only
opening the PR fails, the workspace stays for `shoal pr open --workspace`.
Its JSON is `pr open`'s, with the new `workspace` when Shoal made one.
`review` submits one review with exactly one of `--comment`, `--approve` or
`--request-changes`, a body from `--body` or `--body-file`, and comments on
single lines of the PR's diff: repeated `--line PATH:LINE BODY` on a line of the
PR's version of a file, and `--old-line PATH:LINE BODY` on a removed line,
numbered in the base's version. `--comments-file` adds the comments of a JSON
array, `-` reading standard input, whose objects have `path`, `line`, `body` and
an optional `side` of `new`, the default, or `old`; other fields and values are
refused:

```json
[{"path": "src/a.rs", "line": 42, "body": "Why?"},
 {"path": "src/b.rs", "line": 7, "side": "old", "body": "Keep this"}]
```

Requesting changes needs a body; any other review needs a body, a line comment
or both. Shoal reads the PR's diff at its head commit and refuses the whole
review before sending anything when a comment is on a file the diff does not
change or a line none of its hunks shows, listing each such comment; the review
is then posted of that commit. Scoped processes comment as the agent
account, and their watches do not report back a review they posted. `--approve` and
`--request-changes` use your own login, and for scoped processes, as their
[`pr_review` permission](modes.md#permitted-actions) allows, the host's
[`review_token`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#forges), with no other login to fall back on:
Shoal refuses their verdict, before sending any request, on a host without
one or on a PR its account opened, naming `review_token`, and posts it of the
head commit it read, which an approval also binds. A forge that refuses a
verdict, such as on your own PR, fails with its message. A space's log
records each review it posted, by its verdict. GitHub, Forgejo and Gitea take a review in one request. GitLab drafts a
note on each line, deleting the drafts when one fails, then publishes them with
the body as a summary and requested changes as your reviewer state, then
approves; since publishing takes every draft the account has on the merge
request, a review is refused while others are pending; Bitbucket posts each line comment, then the body, then the verdict.
A failure after part of a review was posted says that it reached the forge.
`issue ls` and `pr ls` list the repository's issues or PRs, newest first,
through the forge's own search: words after the command are text it searches for
in titles and descriptions, which `--match` matches by `keyword`, by `meaning`
through GitHub's search by meaning, or `both` ways by default: every keyword
match and the meaning matches it misses, newest first within `--limit`, or the
keyword matches alone when GitHub's search by meaning fails, such as under its
rate limit, and on a forge without that search, where `meaning` is refused. Repeated `--label` keeps items with every label,
`--author` those a login opened, and `--state open|closed|all` defaults to open,
with merged PRs counted as closed. `--limit` caps the list at 1 to 1000 items,
30 by default. `--repo` searches another repository, named as for
`issue open --repo`; for anyone but a workspace process, one that names a
registered repository is searched through its checkout, needing no workspace,
and one named by its URL needs neither, as `issue open`, `repo labels`, `forge capabilities`, `forge reactions` and
`forge api` use it. Workspace processes search as the agent account, anyone else
with your own logins. Each line shows an item's number, state (`draft` for an
open draft PR), title, labels and each assignee after an `@`; JSON lists the
items with the fields every other item command reports. Bitbucket's `--author`
matches a nickname.
`issue open` takes `--title`, `--body` or `--body-file`, repeated `--label`,
repeated `--assignee` and `--milestone TITLE`;
the new issue is recorded as opened, or with `--link` assigned instead, after the
issues already assigned. A workspace being removed refuses it. A chat
opens it in its repository's registered checkout, under that repository's
settings, and records it the same way. Outside a space it opens the issue in the
repository of the registered checkout it runs in, records it nowhere, and
refuses `--link`. Its JSON
adds `created` and `linked`, and `warnings`, on stderr too, with the
[convention](#repository-conventions) warnings, a failure to label, assign, set
the milestone, attach files or read the issue once the forge opened it, which
still records it with the labels and details the forge reports it has, or as
posted when it cannot be read,
and when removal started before the workspace could record and link the new
issue.

`--repo` opens the issue in another repository, named as `OWNER/NAME` on the
workspace's forge or by a repository URL on any forge, still as the agent
account. The workspace's [conventions](#repository-conventions) do not apply
there, so nothing requires or warns about its labels or description, and
`--link` is refused. The space's `issue_other_repositories`
[permission](modes.md#mode-permissions) decides it for workspace processes: `"propose"` records an [approval request](approvals.md#approval-for-agent-issues-and-prs)
naming the repository, `"direct"` opens it at once, and `"deny"` refuses it.
Unscoped callers always open directly.

### Repository conventions

`shoal repo labels` lists the labels the workspace repository's items can take
as the forge reports them, including a group's or organization's labels, with
each one's description, and `exclusive` for a label that allows no other of its
scope: Forgejo's exclusive labels and GitLab's `key::value` labels. Reading a
Forgejo organization's labels needs a token with the `read:organization`
scope; Bitbucket has no labels. Processes inside a workspace and a chat's agent
read them as the [agent account](agents.md#agent-accounts), anyone else with your own
logins.

[`[pr] required_labels`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#prs) and [`[issue] required_labels`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#issues) name entries that every new
item's labels must each start with, so `area/` takes any area label. `pr open`
and `issue open` refuse a new item missing one before creating it, listing the
repository's labels that would satisfy it; an empty list in a higher layer turns
inherited entries off.

Once the item exists, `pr open`, `issue open` and `ready` warn, without failing,
about an item missing a required label, or about one without labels in a
repository that has some. `pr open` and `issue open` also warn when a
description they sent lacks a heading of the forge's description template in the
worktree: for PRs `pull_request_template.md` in `.forgejo`, `.gitea`, `.github`,
the root or `docs`, or GitLab's `.gitlab/merge_request_templates/Default.md`; for
issues the Markdown files of the first `ISSUE_TEMPLATE` directory or
`issue_template.md` there, or GitLab's `.gitlab/issue_templates`, where a
description that has every heading of any template passes. Names match without
regard to case, front matter is skipped, and a template without headings asks for
nothing. A warning names the `edit` command that fixes it, and one whose lookup
fails is left out.

The daemon sends each change through the forge's REST API as the workspace's
[agent account](agents.md#agent-accounts), except that a [merge](branches.md#merge-into-the-default-branch)
of a PR and its automatic merge run by you use your login. Each change records an
`item_changed` [space event](log.md#space-events). `--json` returns the item
as the forge then reports it: `kind`, `number`, `url`, `title`, `state` (`open`,
`closed` or `merged`), `labels`, and for a PR, `pr` with `head`, `base` and
`draft`.

### Forge API requests

```sh
shoal forge api 'repos/{owner}/{repo}/branches'   # GET a path of the forge's API
shoal forge api 'repos/{owner}/{repo}' -X PATCH --input body.json
shoal forge api 'repos/{owner}/{repo}/labels' --repo team/review-bot  # another repository
```

`shoal forge api` sends a request to a path under the forge API root of the
repository of the workspace, or outside one, such as in a chat, of the
registered checkout it runs in, such as `/api/v1` on Forgejo, and prints the JSON response, which `--json`
prints on one line. `{owner}` and `{repo}` fill in the repository, or the one
`--repo` names as for `issue open --repo`; on GitLab
`{owner}` is the encoded group path, so `projects/{owner}%2F{repo}` names a
nested project. A path that names another host or has a `.` or `..` segment is
refused. The request is a GET, or a POST with `--input`, a JSON body from a file
or `-` for standard input, unless `-X` names another method. It uses the logins,
rate limits and 64 MiB response bound of other forge requests. On Forgejo, a
GET whose query sets `limit` without `page` reads the first page, since
Forgejo's Actions lists otherwise ignore `limit` and return every run.
Processes inside a workspace and a chat's agent send it as the
[agent account](agents.md#agent-accounts), and only GET unless their space's `api_writes`
[permission](modes.md#mode-permissions) lets them send other methods to their space's repository, which could merge, approve or skip required labels
around Shoal's checks: `"direct"` sends them, and `"propose"` makes each an
approval request bound to its method, path and body; to another repository
they send only GET. Anyone
else sends any request with your own logins.

### Forge downloads

```sh
shoal forge download https://github.com/user-attachments/assets/1a2b --output shot.png
shoal forge download "$url" -o - > shot.png
```

`shoal forge download` saves an image or attached file from the forge of the
repository `shoal forge api` would use, such as one in a PR's description, with
the same logins and rate limits, so files of a private repository load; `--repo` names another
repository whose forge it is on. It refuses a URL on any other host before
sending anything, so the login reaches only that forge; on GitHub, Forgejo and
Gitea, redirects to the storage that serves the file get no login. `--output`
names the file to write, or `-` for standard output, and `--json` returns the
written `path`, its size in `bytes` and the `content_type` the forge sent. A
response that is a web page, such as a sign-in page the login cannot pass,
fails instead of being saved, as does a file over 100 MiB. Downloads send the
host's [token](#forges), or go anonymously without one. GitLab's web routes
take no access token, so a file uploaded to a project or group, at
`/-/project/<id>/uploads/<secret>/<file>`, `/<project path>/uploads/…`,
`/-/group/<id>/uploads/…` or `/groups/<group path>/-/uploads/…`, is read from
the API route for the same upload, which GitLab 17.4 added; other GitLab URLs
are refused. Bitbucket serves its files only to a signed-in browser, so it has
no downloads. The `downloads` capability says whether the forge has them.
