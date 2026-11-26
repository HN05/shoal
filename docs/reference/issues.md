# Issues and pull requests

This file says what the `issue`, `pr` and `forge` commands do and which forges
Shoal supports. [Forges](../design/forge.md) gives the reasons.

## Forges

Shoal reads and changes the issues and PRs of the forge that serves a
repository's `origin`. It knows these hosts by name:

- `github.com` is GitHub,
- `gitlab.com` is GitLab,
- `bitbucket.org` is Bitbucket Cloud.

The daemon asks any other host once which API it serves, and records the
answer. It asks with the host's [token](#logins), through GitHub's `meta`,
GitLab's `version` and Forgejo's or Gitea's `version` on that host. It asks
recorded hosts again when it starts or reloads its config. When a host now
answers as another forge, the daemon records an alert.

A host that answers as none of them fails until you set its
[`type`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#forges).
Shoal never asks a host whose `type` is set.

Shoal sends every request to the forge's API with curl. It runs no forge CLI.
The API is at:

- `api.github.com` for GitHub.com,
- `/api/v3` for a GitHub Enterprise Server,
- `/api/v4` for GitLab,
- `/api/v1` for Forgejo and Gitea,
- `api.bitbucket.org` for Bitbucket Cloud.

### Logins

Shoal signs each request in with one of the host's tokens, which
[`[machine.forges]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#forges)
sets. [Agent accounts](agents.md#forge-requests) says when a request uses the
agent account's `agent_token` and when your own `user_token`. A PR merge or
automatic merge that you run in a space uses your own login.

### Capabilities

Forges differ in what Shoal can do there. `shoal forge capabilities` lists each
capability of the repository's forge, such as resolving review threads,
rerunning CI jobs, labels or a merge method. For each, it says whether the forge
has it, and what to do instead when it does not. Where a forge's releases
differ, Shoal asks the server.

`--json` returns `forge` and `capabilities`. Each capability has `capability`,
`supported` and an optional `hint`.

A command that needs a capability the forge lacks fails before it changes
anything. The error names the forge and the capability.

### GitLab

- A project may sit in subgroups.
- Merge requests are PRs. Item URLs use `/-/issues/` and `/-/merge_requests/`.
- Reviewers are usernames.
- CI checks are the latest status of each job and external check on the head
  commit.
- Reviews are reviewers' states and approvals.
- Shoal does not model approval rules or merge trains.

### Bitbucket

- A repository lives under a workspace. PR URLs use `/pull-requests/`.
- Bitbucket reports no merge conflicts.
- Reviewers are Atlassian account IDs.
- CI checks are the builds reported on the PR.
- Reviews are participants' approvals and change requests.

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

## Where commands act

`shoal pr` and `shoal issue` act in the repository of the space they run in, a
workspace or a chat, or of the space `-s`/`--space` names. A chat acts in its
repository's registered checkout.

A command changes the item that its number or URL names. Without one, it
changes the space's only assigned issue or PR. A space with several needs the
number or URL. A number may be written `#42`.

Workspace processes act only from their own workspace. `pr open` needs a
workspace, and `pr revert` a space.

Commands that need only a repository, such as `issue ls`, `repo labels` and the
`forge` commands, read the repository of the workspace or registered checkout
they run in, a chat's included.

### Outside a space

Outside a space, the commands act in the repository of the registered checkout
they run in. They take only a number or URL.

Outside any checkout, a terminal offers a picker:

- `shoal pr` and `shoal issue` commands that act on an item, or open an issue,
  offer the spaces.
- Commands that need only a repository offer the registered repositories.

Without a terminal, the call fails and says so.

### Another repository

`-r`/`--repo`, instead of `--space`, names another repository:

- a registered repository's name,
- `OWNER/NAME` on the current repository's forge,
- a repository URL on any forge.

An issue or PR URL names its own repository. Both work from anywhere, for a
repository registered or not, so a client acting as you needs no workspace.
Shoal acts on a registered repository through its checkout.

Anyone reads another repository's items. Changing them is yours, with your own
logins: a scoped process's change there is refused.
[`issue open --repo`](#open-an-issue-in-another-repository) is the exception.

## Issue and PR actions

```sh
shoal pr open --label area/cli      # push, then link the branch's open PR or open one
shoal pr edit --title "Fix login" --add-label bug --draft
shoal pr comment --body-file notes.md
shoal pr comment --body-file notes.md --attach shot.png --attach 'demo.mp4#Login flow'
shoal pr comment --edit 812 --body-file fixed.md  # replace the body of comment 812
shoal pr reply 812 --body-file why.md   # in the thread of review comment 812
shoal pr resolve 812 --body-file why.md # reply, then resolve; unresolve reopens
shoal pr react +1 --comment 812
shoal pr close 12                   # reopen works the same way
shoal pr reviewers 12               # users and teams who can review PR 12
shoal pr update --method rebase     # take in the base's commits on the forge
shoal pr comment 12 --repo shoal --body-file notes.md  # in a registered repository, without a workspace
shoal issue edit --add-label bug    # comment, close and reopen work too
shoal issue edit 12 --add-assignee kim --milestone v2  # --clear-milestone removes it
shoal issue open --title "Flaky watch test" --label bug
shoal issue open --repo team/review-bot --title "Missed finding" # in another repository
```

Each change records an `item_changed` [space event](log.md#space-events).
`--json` returns the item as the forge reports it after the change:

- `kind`, `number`, `url` and `title`,
- `state`: `open`, `closed` or `merged`,
- `labels`,
- for a PR, `pr` with `head`, `base` and `draft`.

### Open a PR

`shoal pr open` first checks the workspace's worktree identity, and that the
worktree is on its recorded branch. It then pushes the branch, without forcing,
to its push remote: `branch.<name>.pushRemote`, else `remote.pushDefault`, else
`origin`. The push sets the branch's upstream. It runs as a tracked command
with your Git configuration and hooks, and prints its output on stderr.

The daemon then links the open PR whose head is that branch, in the repository
the push remote names. Without one, it opens a PR in the `origin` repository and
links it.

A push remote on the same forge host other than `origin` is a fork, and the PR
comes from it. GitLab merge requests from a fork are refused.

A new PR takes these defaults:

- Its title is `--title`, else the first assigned issue's title, else the last
  commit's subject.
- Its description is `--body` or `--body-file`, else a `Closes #<number>` line
  for each assigned issue.
- Its base is `--base`, else the base workspace's branch, else the default
  branch.
- It is a draft with `--draft` or
  [`[pr] draft = true`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#prs).
  `--no-draft` overrides that.

Draft state, repeated `--label`, repeated `--reviewer`, repeated `--assignee` and
`--milestone TITLE` apply only to a new PR.

`--json` adds `created`, false for a PR it found, `linked` and `warnings`. The
warnings also go to stderr. They are:

- the [convention](#repository-conventions) warnings,
- a failure to label, request reviews, assign, set the milestone or attach files
  once the forge opened the PR,
- a failure to read the new PR.

Shoal still records and links a PR with such a failure. It records the labels
and details the forge reports, or those it posted when it cannot read the PR.

### Open an issue

`shoal issue open` takes `--title`, `--body` or `--body-file`, repeated
`--label`, repeated `--assignee` and `--milestone TITLE`.

The space records the new issue as opened. With `--link`, it assigns the issue
instead, after the issues already assigned. A workspace being removed refuses
the command.

A chat opens the issue in its repository's registered checkout, under that
repository's settings, and records it the same way. Outside a space, the issue
opens in the repository of the registered checkout the command runs in. Shoal
records it nowhere there, and refuses `--link`.

`--json` adds `created`, `linked` and `warnings`. The warnings also go to
stderr. They are:

- the [convention](#repository-conventions) warnings,
- a failure to label, assign, set the milestone or attach files once the forge
  opened the issue,
- a failure to read the new issue,
- removal that started before the workspace could record and link the new
  issue.

Shoal still records an issue with such a failure. It records the labels and
details the forge reports, or those it posted when it cannot read the issue.

#### Open an issue in another repository

`--repo` opens the issue in another repository: `OWNER/NAME` on the workspace's
forge, or a repository URL on any forge. A scoped process still opens it as the
agent account.

The workspace's [conventions](#repository-conventions) do not apply there.
Nothing requires or warns about its labels or description. `--link` is refused.

The space's `issue_other_repositories` permission decides it for a workspace
process, as [approval for agent issues and
PRs](approvals.md#approval-for-agent-issues-and-prs) says. A proposal names the
repository.

### Edit an issue or PR

`edit` takes these options, and applies them in this order:

- `--title`,
- `--body` or `--body-file`,
- repeated `--add-label` and `--remove-label`,
- for a PR, `--base`,
- for a PR, repeated `--add-reviewer` and `--remove-reviewer`,
- for a PR, `--draft` or `--no-draft`,
- repeated `--add-assignee` and `--remove-assignee`,
- `--milestone TITLE` or `--clear-milestone`.

A failed request stops the rest, with the forge's message.

`--add-reviewer` and `--remove-reviewer` take a team as `owner/team` on GitHub
and Forgejo.

Forgejo keeps a PR's draft state as the title's `WIP: ` prefix, and GitLab as
its `Draft: ` prefix.

#### Labels

Shoal replaces an item's labels as a set, by name. An added exclusive label
replaces the item's label of its scope.

An edit fails when the item then lacks a label it added, or still has a label
it removed. The error names those labels and the item's labels. It also names
each added label that the repository has no label for.

#### Assignees and milestones

Shoal replaces an item's assignees as a set, by login. A milestone is named by
its exact title, open or closed. On GitLab, a group's milestone counts too.

An edit fails when the item then does not show its assignees or milestone. The
error names them and what the item has. A GitHub user who cannot be assigned
fails this way, as does a second assignee on GitLab's free tier.

A Bitbucket issue takes one assignee, by account ID. A Bitbucket PR takes
neither assignees nor milestones.

### Close and reopen

`close` closes an issue or PR, and `reopen` reopens it. On Bitbucket, closing a
PR declines it.

### Reviewers

`shoal pr reviewers` lists the users and teams that a PR can ask for a review,
except its author. It names each by the `name` that `--add-reviewer` takes,
with the forge's `display_name`, and says whether it is a `team`.

- Users are GitHub's assignable users, Forgejo's review candidates, GitLab's
  project members and the members of a Bitbucket workspace.
- Teams are those with access to an organization's repository, on GitHub and
  Forgejo.

### Attach files

`open`, `edit` and `comment` take repeated `--attach FILE` or
`--attach 'FILE#ALT TEXT'`. Shoal checks each file before it sends anything. A
file must not be empty, and must be at most 10 MiB for an image or 100 MiB
otherwise. The forge decides which types it accepts.

Shoal uploads each file through the forge. The upload replaces the body's image
references to the file, such as `![The error](shot.png)`. A reference is
relative to the body file's directory or the working directory. A file the body
does not reference follows the body. `edit` without a new description adds the
files after the current one.

A PNG, JPEG, GIF, WebP or SVG image is shown, as is an MP4, MOV or WebM video.
Any other file is linked, with its alt text or its name.

Uploads need write access to the repository on GitHub, and a token on Forgejo.

GitHub and GitLab keep uploads with the repository. Forgejo and Gitea keep them
with the item or comment. There, Shoal first posts a new comment or item that
names the files, then updates it to show them. It first refuses a file that the
instance's attachment settings would refuse, by type or size.

Shoal resends an upload up to three times while the connection fails before
sending it, or while the forge answers 503. It resends a comment's update also
after a broken connection or another 5xx. A 429 is the forge's
[rate limit](#rate-limits-and-retries), which holds back requests until it ends.

### Comments and review threads

`comment` posts a comment from `--body` or `--body-file`.

`comment --edit ID` replaces the body of a comment, or on a PR of an inline
comment, by its ID as `shoal view` shows it. It takes `--body` or `--body-file`.
Its `--attach` files are uploaded and shown as a new comment's are, after the
current body when no new one is given. Before it sends anything, Shoal checks
that the comment belongs to the item and that the login Shoal acts as wrote it.
A comment that already has the new body stays unchanged. Watches do not report a
workspace process's edit back.

`pr reply`, `pr resolve` and `pr unresolve` take the ID of any comment in a
review thread, as `shoal view` shows it, before the PR's number or URL:

- `reply` answers in the thread, from `--body` or `--body-file`.
- `resolve` resolves the thread. Given a body, it first posts the body as a
  reply.
- `unresolve` reopens the thread.

Resolving a thread that has no reply asks for confirmation. `--yes` skips it.

### Reactions

`react REACTION` adds a reaction to the item. `--comment ID` reacts to one of
its comments or inline comments instead, by its ID as `shoal view` shows it.
`--remove` removes the reaction.

A reaction is GitHub's name for it, as `shoal view` shows it, or one the forge
has, such as a GitLab emoji. Before it sends anything, Shoal checks that the
comment belongs to the item, of the item's kind. A reaction already as asked
stays unchanged. Bitbucket has no reactions.

`shoal forge reactions` lists the reactions the forge allows, under the names
`shoal view` reports:

- on Forgejo and Gitea, those the instance's settings allow,
- on GitHub, its eight,
- on GitLab, the same eight as suggestions. Any emoji GitLab knows works.

`--json` returns `names` and `accepts_other_names`.

### Update a PR's branch

`pr update` brings an open PR's branch up to date with its base, on the forge:

- `--method merge`, the default, merges the base in.
- `--method rebase` rebases the branch onto the base.

Workspace processes may update. They then pull the branch before they push
again.

GitHub refuses the update when the head moved since Shoal read it. GitLab only
rebases, and rebases after it answers. Bitbucket cannot update a branch.

## Merge a PR

```sh
shoal merge 12 -X rebase --delete-branch  # merge PR 12 on the forge
shoal merge pr --auto               # merge the linked PR once checks pass; --cancel-auto withdraws it
```

`shoal merge` merges a PR on the forge.
[Merge into the default branch](branches.md#merge-into-the-default-branch) says
which PR it picks, and how it merges a workspace without one.

### Method

The method is the first of these:

- `-X`/`--method merge|rebase|squash`,
- [`[merge] method`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#merges)
  of the repository it acts in, or of the global config for another repository,
- the repository's default.

The repository's default is:

- on Forgejo and Gitea, the default merge style,
- on GitLab, the merge method and squash setting,
- on Bitbucket, the default strategy,
- on GitHub, which has none, the first of merge, squash and rebase that the
  repository allows.

On Bitbucket, `rebase` rebases the branch and fast-forwards.

`--message` gives a squash merge its commit message. Its first line is the
subject, where the forge takes the subject apart. `--message` is refused for a
merge by another method, and for a default method the forge does not tell.

`--delete-branch` deletes the PR's branch on the forge when the branch is in the
repository. It never deletes a local branch.
[`shoal branch delete`](branches.md#branches-on-the-forge) deletes a PR's branch
on the forge once the PR has merged or closed.

### Automatic merge

`--auto` has the forge merge the PR by that method once its required checks and
approvals pass. It merges at once when they already pass. It uses:

- GitHub's auto-merge,
- GitLab's merge when the pipeline succeeds,
- Forgejo's merge when checks succeed.

Bitbucket cannot merge later. On GitHub, the repository's settings decide
whether an automatic merge deletes the branch, and `--auto` refuses
`--delete-branch`.

`--cancel-auto` withdraws the automatic merge.

### The head commit

A merge and an automatic merge name the head commit Shoal read for them. The
forge refuses one whose head moved since. Bitbucket takes no head commit.

Only GitLab keeps an automatic merge bound to that head until it merges. It
drops the merge when the branch moves. The other forges merge the head the PR
has once checks pass. `shoal forge capabilities` says which applies.

### Files changed on both sides

A workspace that links the PR records its last check for
[files changed on both sides](branches.md#files-changed-on-both-sides). When
that check found such files at the PR's head and its base branch, a merge and
`--auto` name them and ask before merging:

```text
Sensitive files changed on both sides by PR #12 and main: src/version.rs
```

`--yes` merges without asking. Without a terminal, the merge refuses and names
`--yes`.

A merge reads only that record. It merges without a question in these cases:

- the PR has no record,
- the record is of another head or base branch,
- no workspace links the PR.

A merge is refused when its warning changed while the question was open. That
includes a warning that appeared where there was none.

### An agent's merge

A scoped process merges, or sets or withdraws an automatic merge, as the agent
account. Its space's [`merge` permission](modes.md#mode-permissions) decides
whether it may. A request binds:

- the PR at its head commit, which the merge names to the forge,
- the method, from `-X` or `[merge] method`,
- a squash's message.

With `"propose"`, a later commit, another method or another message needs
another approval.

## Revert a PR

```sh
shoal pr revert 42                  # open a PR that reverts merged PR 42
```

`shoal pr revert` opens a PR that undoes a merged PR, into the merged PR's base
branch. It refuses a PR that has not merged.

It takes `pr open`'s options, except `--base`. The new PR's defaults are:

- title: `Revert "<title>"`,
- description: `Reverts #<number>`,
- labels: the reverted PR's.

`--json` returns what `pr open` returns, with the new `workspace` when Shoal
made one. [Merges and reverts](approvals.md#merges-and-reverts) says when a
scoped process may revert.

### Where the forge reverts

Where the forge reverts PRs, the forge opens the PR from a branch it creates.
Only GitHub does, which the `revert_prs` capability reports. The space records
the PR as opened, without linking it.

### Where Shoal reverts

Elsewhere, Shoal creates a workspace on branch `revert-<number>`, from the base
branch as `origin` has it. Setup runs as for any new workspace. Where the new
workspace sits depends on who reverts:

- A workspace process's revert makes it a child of the process's workspace.
- A chat's revert makes it a child of the chat.
- Your own revert from a workspace makes it a top-level workspace.

Shoal reverts the merge in one commit, as tracked commands of that workspace.
It first establishes which commits the merge added. The merge is one of these:

- a merge commit whose later parent has the PR head's tree, reverted against
  its first parent,
- the base's latest first-parent commits, when they carry the patches of the
  PR's commits, as after a rebase or fast-forward,
- one squashed commit whose tree is the PR's head merged onto its parent.

Once the PR's branch is gone, Shoal fetches its commits from the PR's head ref
on `origin`.

The revert is refused when Shoal cannot establish the merge, or when setup left
changes to tracked files.

Shoal then pushes the branch and opens the PR as `pr open` does, linked to the
new workspace. It marks that workspace done with cleanup, so the workspace goes
once the revert merges.

A revert that conflicts, or that changes nothing, removes the new workspace and
fails. The error lists the conflicting files. When only opening the PR fails,
the workspace stays for `shoal pr open --workspace`.

## Review a PR

```sh
shoal pr review 42 --comment --body-file review.md --line src/a.rs:7 'Why?'
shoal pr review 42 --request-changes --body-file why.md --comments-file lines.json
```

`shoal pr review` submits one review with exactly one of `--comment`,
`--approve` or `--request-changes`. Its body comes from `--body` or
`--body-file`. Requesting changes needs a body. Any other review needs a body,
a line comment or both.

### Line comments

Each line comment is on a single line of the PR's diff:

- `--line PATH:LINE BODY`, repeated, comments on a line of the PR's version of
  a file.
- `--old-line PATH:LINE BODY` comments on a removed line, numbered in the
  base's version.
- `--comments-file` adds the comments of a JSON array, with `-` for standard
  input.

Each object of the array has `path`, `line`, `body` and an optional `side`:
`new`, the default, or `old`. Other fields and values are refused:

```json
[{"path": "src/a.rs", "line": 42, "body": "Why?"},
 {"path": "src/b.rs", "line": 7, "side": "old", "body": "Keep this"}]
```

Shoal reads the PR's diff at its head commit. Before it sends anything, it
refuses the whole review when a comment is on a file the diff does not change,
or on a line none of its hunks shows. The error lists each such comment. Shoal
posts the review of that commit.

### Who reviews

Scoped processes comment as the agent account. Their watches do not report back
a review they posted.

You approve and request changes with your own login. A scoped process does so
as its [`pr_review` permission](modes.md#permitted-actions) allows, with the
host's
[`review_token`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#forges).
It has no other login to fall back on. Before it sends any request, Shoal
refuses the verdict, naming `review_token`, in these cases:

- the host has no `review_token`,
- the `review_token`'s account opened the PR.

Shoal posts a verdict of the head commit it read, which an approval also binds.
A forge that refuses a verdict, such as on your own PR, fails with its message.
A space's log records each review it posted, by its verdict.

### How each forge takes a review

- GitHub, Forgejo and Gitea take a review in one request.
- GitLab takes a draft note on each line. Shoal deletes the drafts when one
  fails. It then publishes them, with the body as a summary, sets requested
  changes as your reviewer state, and then approves. A review is refused while
  the account has other drafts pending on the merge request.
- Bitbucket takes each line comment, then the body, then the verdict.

A failure after part of a review was posted says that the review reached the
forge.

## Search issues and PRs

```sh
shoal issue ls --label bug flaky  # search by text; --state, --author and --limit too
shoal pr ls --state all --author dev
shoal issue ls --repo team/review-bot missed  # search another repository
```

`issue ls` and `pr ls` list the repository's issues or PRs, newest first,
through the forge's own search. Words after the command are text to search for
in titles and descriptions. `--match` says how the text matches:

- `keyword` matches its words.
- `meaning` matches through GitHub's search by meaning. Another forge refuses
  it.
- `both`, the default, gives every keyword match and the meaning matches that
  keyword search misses, newest first within `--limit`.

`both` gives the keyword matches alone on a forge without search by meaning,
and when GitHub's search by meaning fails, such as under its rate limit.

These options filter the list:

- `--label`, repeated, keeps items with every label.
- `--author` keeps the items a login opened. On Bitbucket it matches a
  nickname.
- `--state open|closed|all` defaults to `open`. A merged PR counts as closed.
- `--limit` caps the list at 1 to 1000 items, 30 by default.

Each line shows an item's number, its state, its title, its labels and each
assignee after an `@`. The state of an open draft PR is `draft`. `--json` lists
the items with the fields every other item command reports.

## Repository conventions

```sh
shoal repo labels                        # the repository's labels with their descriptions
shoal repo labels --repo team/review-bot # another repository's
```

### Labels

`shoal repo labels` lists the labels the repository's items can take, as the
forge reports them, a group's or organization's included. It shows each label's
description. It marks `exclusive` a label that allows no other of its scope:
Forgejo's exclusive labels and GitLab's `key::value` labels.

Reading a Forgejo organization's labels needs a token with the
`read:organization` scope. Bitbucket has no labels.

### Required labels

`pr open` and `issue open` refuse a new item that lacks a label
[`[pr] required_labels`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#prs)
or
[`[issue] required_labels`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#issues)
asks for. They refuse it before they create it. The error lists the
repository's labels that would satisfy each missing entry.

### Warnings

Once the item exists, `pr open`, `issue open` and `ready` warn about these,
without failing:

- an item that lacks a required label,
- an item without labels, in a repository that has some.

`pr open` and `issue open` also warn when a description they sent lacks a
heading of the forge's description template in the worktree. The templates are:

- for PRs, `pull_request_template.md` in `.forgejo`, `.gitea`, `.github`, the
  root or `docs`, or GitLab's `.gitlab/merge_request_templates/Default.md`,
- for issues, the Markdown files of the first `ISSUE_TEMPLATE` directory in
  those places, or `issue_template.md` there, or GitLab's
  `.gitlab/issue_templates`.

An issue description passes when it has every heading of any one template.
Headings match without regard to case. Shoal skips front matter, and a template
without headings asks for nothing.

A warning names the `edit` command that fixes it. Shoal leaves out a warning
whose lookup fails.

## Forge API requests

```sh
shoal forge api 'repos/{owner}/{repo}/branches'   # GET a path of the forge's API
shoal forge api 'repos/{owner}/{repo}' -X PATCH --input body.json
shoal forge api 'repos/{owner}/{repo}/labels' --repo team/review-bot  # another repository
```

`shoal forge api PATH` sends a request to a path under the forge's API root,
such as `/api/v1` on Forgejo, and prints the JSON response. `--json` prints it
on one line.

`{owner}` and `{repo}` fill in the repository. On GitLab, `{owner}` is the
encoded group path, so `projects/{owner}%2F{repo}` names a nested project. A
path that names another host, or has a `.` or `..` segment, is refused.

The request is a GET. With `--input`, it is a POST whose JSON body comes from a
file, or from standard input with `-`. `-X` names another method.

The request uses the logins, the rate limits and the 64 MiB response bound of
other forge requests. On Forgejo, a GET whose query sets `limit` without `page`
reads the first page.

Processes inside a workspace and a chat's agent send the request as the
[agent account](agents.md#agent-accounts). They send only GET, unless their
space's `api_writes` [permission](modes.md#mode-permissions) lets them send
other methods to their space's repository:

- `"direct"` sends them.
- `"propose"` makes each an approval request, bound to its method, path and
  body.

To another repository, they send only GET. Anyone else sends any request with
your own logins.

## Forge downloads

```sh
shoal forge download https://github.com/user-attachments/assets/1a2b --output shot.png
shoal forge download "$url" -o - > shot.png
```

`shoal forge download URL` saves an image or attached file from the forge, such
as one in a PR's description. It uses the forge of the repository
`shoal forge api` would use, or of the one `--repo` names. It signs in with the
same logins, under the same rate limits, so files of a private repository load.
Without a [token](#logins), it goes anonymously.

It refuses a URL on any other host before it sends anything. On GitHub, Forgejo
and Gitea, redirects to the storage that serves the file get no login.

`--output` names the file to write, or `-` for standard output. `--json`
returns the written `path`, its size in `bytes` and the `content_type` the forge
sent.

A response that is a web page, such as a sign-in page the login cannot pass,
fails instead of being saved. So does a file over 100 MiB.

On GitLab, Shoal reads a file uploaded to a project or group from the API route
for the same upload, which GitLab 17.4 added. It takes these URLs, and refuses
other GitLab URLs:

- `/-/project/<id>/uploads/<secret>/<file>`,
- `/<project path>/uploads/…`,
- `/-/group/<id>/uploads/…`,
- `/groups/<group path>/-/uploads/…`.

Bitbucket has no downloads. The `downloads` capability says whether a forge has
them.
