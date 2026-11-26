# Forges

This file covers `shoal-forge`: which forge serves a host, the accounts
requests act as, item references, forge reads and writes, branch deletion and
other repositories. The [issues reference](../reference/issues.md) says what
the `issue`, `pr` and `forge` commands do.

## Requests through the daemon

Shoal owns the forge requests of its spaces. Every client, from an agent in a
terminal to an app built on Shoal, thus gets one item model, one account rule
and one policy. Every read and change runs in the daemon, where scope applies.
A client chooses neither a login nor a host's forge, and the CLI resolves no
forge host of its own.

Shoal sends REST requests with curl, and GitHub's GraphQL requests the same
way. It needs only a token per host, so it runs no forge CLI and reads none of
their login files. A forge thus works without its CLI installed. Without a
token, only reads go out, anonymously.

Browsing across repositories and review tooling stay with clients.

## Accounts

A scoped caller acts as the agent account, the host's `agent_token`. The user,
and any other unscoped caller, acts with the user's own logins.
[Agent accounts](../reference/agents.md#agent-accounts) says which login each
request uses. The one exception is the user's own login among an issue's
assignees, which Shoal adds and removes with the user's token, as
[assignees on the forge](links.md#assignees-on-the-forge) explains.

Without an `agent_token`, the agent reads with the user's token, as the agent
itself could, but changes nothing. No change goes out as the user unless
`agent_token = { same_as = "user_token" }` says so. Shoal refuses a change to a
host without a token for its account before it sends it, naming the setting and
the reload the daemon needs. A forge would answer the unsigned change only with
a bare 401.

Merging and a review's verdict are the user's decisions. A permission gives
them to a scoped caller: `merge` its merges, as the agent account, and
`pr_review` its verdicts, as the host's `review_token`. That token is a login of
its own, because agents open their PRs as the agent account and forges take no
verdict from a PR's author.

### Agents' Git

An agent's Git over HTTPS to a host with an `agent_token` acts as the agent
account too, as do the pushes Shoal runs for it. A credential helper gives Git
the token, so the token never enters a file, the environment or argv. The
agent's Git settings scope that helper to the host and clear the user's helpers
for it. Git asks the helper before its first request, ahead of a login of the
user's `.netrc` for the host.
[Git over HTTPS](../reference/agents.md#git-over-https) says what it covers.

## Which forge a host serves

Shoal selects the forge from the remote's host without configuration. It knows
well-known hosts by name. It asks any other host once, through each forge's
version or metadata endpoint, since a name cannot tell a GitHub Enterprise
Server or a self-hosted GitLab from Forgejo. The question carries the user's
token for that host. A token goes only to the host it is set for.

The daemon records each answer in its state. It asks recorded hosts again when
it starts or reloads its config, and records a changed answer with an alert. A
host it cannot reach keeps its answer and raises no alert, since a machine may
start offline.

A host's
[`type`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#forges)
pins its forge, and Shoal never asks that host. A host that answers as none
fails with a message naming that setting. Types and tokens describe hosts
rather than repositories, so only the global config sets them.

Each process remembers the answers it resolved, so the links of a host parse
the same way. The CLI starts out knowing the pinned ones. A pasted link of a
host not yet resolved is classified without a request, by the forge its route
names, such as `/pull/` for GitHub and `/-/merge_requests/` for GitLab. Several
forges share the form of an issue or repository URL, so such a URL falls back
to the host's name: GitLab for a `gitlab` name label, and otherwise Forgejo.

## Item references

An issue, PR or CI run is read once, where it enters, into an item reference of
its repository, kind and number. It enters as a command's argument, a protocol
field, a stored link as it loads, or a forge's answer. Code past that point
passes the reference and never reads its text again.

Stores key items by their canonical URL text, which each row reads back into a
reference as it loads. A number names an item only once the repository it
belongs to is known.

## One model across forges

Issue and PR actions are typed, such as edit, comment, close, reopen and merge.
Each forge's backend plans an action as that forge's requests. What a forge does
its own way stays in that planning, so callers see the same item on every forge.
Forgejo, for example, keeps a PR's draft state in its title.

Shoal models what forges share. GitLab-only concepts, such as approval rules and
merge trains, stay unmodeled, and approvals count as approving reviews.

Options are modeled rather than passed through to a forge, so they mean the
same on every forge. A milestone is named by its title, which the backend looks
up as the ID its forge takes. Labels and reviewers are listed under the names
those options take, a team as `owner/team`. A client thus offers them without
forge requests of its own. Reactions use GitHub's names on every forge, so a
client offers one set, which `shoal forge reactions` narrows to what an instance
allows.

Hiding a difference can cost requests. Forgejo's issue search lists PRs without
their branches, so each listed PR is read in full. GitLab keeps merged merge
requests apart from closed ones, so listing closed ones reads all and drops the
open ones.

### Capabilities

Each backend declares the capabilities it has. Shoal refuses a request that
needs one the forge lacks before it sends anything, with one typed error that
names the forge and the capability. A backend thus gains a capability without
changes elsewhere. `shoal forge capabilities` reports them, so agents can check
first.

Where a forge's releases differ in a capability, the backend asks the server
for its version before the request. An older server thus refuses as clearly,
and the report matches the server.

## Changes

Each action returns the item as the forge reports it afterwards, and records an
`item_changed` event for integrations.

A new item gets its assignees and milestone from the edit that finishes it, so
one path sets them. An edit fails when the item the forge then reports lacks its
labels, assignees or milestone, because forges drop without an error what they
cannot apply.

### Opening issues and PRs

A new issue is recorded as opened, and as assigned only on request. A space's
assigned issues are its work, while an issue an agent opens is usually follow-up
work.

A new PR is ready unless the caller asks for a draft, because opening a ready PR
is what requests review. `shoal pr open` pushes, then finds or opens the
branch's PR and links it in one daemon request. Such a PR stays unlinked only
when the space's removal started after the request's check, which the result
reports as a warning. When assigning a PR it created fails, the space records it
as opened.

Repository conventions reach agents where they open and mark items, not only in
the instructions they read at launch. `shoal repo labels` serves the forge's
label scheme with its descriptions. Required labels refuse a new item before it
exists. Missing labels or template headings come back as warnings, which never
fail a change that already happened. Shoal never fills a template in, because an
unfilled template says less than a closing reference.

### Comments and reactions

A comment edit changes only a comment the acting login wrote. Shoal checks that
against the item's discussion before it sends anything. An account with admin
rights could otherwise rewrite someone else's words.

Reacting reads the reactions first and changes only one not already as asked,
since forges differ in whether adding one twice fails. A view reads reactions
with its comments, which costs a request per comment on forges that list them
per comment.

### Attached files

The daemon reads attached files from paths the CLI checked, anywhere on disk. A
body file can already come from anywhere, and scope is cooperative. Recordings
kept in the worktree instead would be untracked files that hold up cleanup.

GitHub's REST API has no upload endpoint, so Shoal uses the one gh uses, on
GitHub's upload host.

Forgejo takes a comment's files only once the comment is posted, so a failure
there would leave it without them. Shoal therefore first refuses what the
instance's attachment settings refuse. It detects a file's media type from its
leading bytes, as Forgejo does. It resends the uploads and the comment's update
after failures that may pass. It resends an upload only when the forge did not
get it or answered 503: a second copy would stay on the comment, and a proxy's
other 5xx may follow a stored one. A comment whose files still fail stays
posted, neither deleted nor finished later, and its error says so.

### Reviews

A review's line comments are checked against the PR's diff before anything is
sent, so a review is refused whole rather than posted in part. GitLab publishes
every draft note the account has on a merge request at once. A review there is
therefore refused while other drafts are pending, rather than publishing them
with it.

A forge that refuses the user's verdict, such as on the user's own PR, says so
itself; Shoal does not guess first. A scoped verdict is refused before any
request when the host has no `review_token` or its account opened the PR, so
the refusal names the setting to change.

Review threads are named by any of their comments' IDs. Shoal does not sign in
to a forge's web UI, so it cannot resolve threads on Forgejo, whose API has no
endpoint for it. Resolving a thread nobody has answered needs confirmation,
because the reply tells the reviewer why.

### Merges

A PR merge, now or automatic, names a head commit: the one a scoped request's
approval bound, else the one Shoal read before merging. The forge thus refuses a
head pushed since, instead of merging a commit nobody approved. Bitbucket takes
no head commit.

A forge checks that head when an automatic merge is set. Only GitLab keeps it
bound to that head until the merge. Elsewhere, a later push still merges, and
the `auto_merge_at_head` capability tells the forges apart.

### Reverts

A revert uses the forge's own where it has one. Elsewhere Shoal reverts the
merge with Git in a new workspace, which opens the PR and goes once it merges.
Backing out a merge thus needs no agent. A conflicting revert would need one, so
Shoal abandons it rather than leave it half done.

Shoal establishes which commits the merge added from their trees and patches,
and never assumes them. Reverting a guess could undo someone else's work.

## Reads

A scoped caller reads as the agent account, and anyone else as the user's own
logins. This holds in another repository too, since checking for a duplicate
before filing an issue there is only a read.

A search uses each forge's own search, and reads pages only until the limit is
met. A search for words matches them both ways by default. GitHub's search by
meaning misses exact matches that keyword search finds, such as an error
message or a flag, so `both` adds its matches to the keyword ones. When search
by meaning fails, such as under GitHub's stricter rate limit for it or on a
server without it, `both` gives the keyword matches alone. The default thus
never fails a search that keyword matching answers.

A forge's rate limit for some requests, such as GitHub's for searches, holds
back only those requests, never the watches and reads the others serve. A pause
the forge puts on the whole account, such as a `Retry-After`, holds back every
request.

### Repeated reads

Forge reads go through one request layer in the daemon. Polls repeat the same
reads, so the layer keeps them cheap and steady.
[Rate limits and retries](../reference/issues.md#rate-limits-and-retries) says
what it does.

The layer remembers each account's last response per URL and repeats the read
as a conditional request. An unchanged item thus costs a `304 Not Modified`,
which GitHub does not count against its primary rate limit. The memory is not
persisted, so a restart reads everything once in full.

The layer reads each forge's rate-limit headers. It sends nothing to a forge
that limits the account until the limit ends. A 429 is such a limit, so the
layer never resends it within seconds as it does a 503.

The layer sends a read again after a failure that may pass. Reading twice
changes nothing, and one refused connection would otherwise fail a whole poll.

### API requests

`shoal forge api` passes any request under the repository's forge API root
through the daemon, for reads no command models. The path stays under the API
root, so the login reaches only the API of the forge its repository is on.

The daemon cannot tell what a method other than GET changes. A scoped caller
therefore sends one to its own repository only as its `api_writes` permission
allows. `"direct"` accepts that its writes may merge, approve or skip required
labels, and `"propose"` asks for each write, bound to its method, path and body.
To another repository it sends only GET, since that setting does not speak for
it.

### Downloads

`shoal forge download` reads a file, such as an image in a description, through
the daemon the same way. A client without forge logins thus shows a private
repository's images. It takes only URLs on the repository's forge host. The
daemon sends the bytes back in frames, since the caller, not the daemon, chooses
where they go.

curl sends the token only to that host, never to the object storage a redirect
leads to. GitLab's web routes for uploads take no access token, so its backend
maps an upload URL to the API route for the same file. It reads that route with
the host's token, like every other GitLab request. Bitbucket's files need a
browser session, so it has no downloads.

## Branch deletion

`shoal branch delete` deletes branches on the forge. The daemon runs it for a
chat as for a workspace, since a chat has no `git push --delete`. The `push`
permission governs it as it governs a pushed deletion, with one `"propose"`
request per branch, bound to the commit the branch points to.

The daemon deletes each branch with a Git push over HTTPS as the caller's
account, leased at that commit. The lease makes the forge keep a branch that a
push moved after the checks. The forges' APIs cannot, since they delete a branch
by name. The token reaches
Git on standard input and stays in Git's credential cache for that host alone,
never in a file, the environment or argv.

The daemon checks every named branch before it deletes any.
[Delete branches](../reference/branches.md#delete-branches) lists what it
refuses. A space's branch is active while the space exists without a recorded
completion and the default branch lacks its changes. A finished space's branch
goes as any other. The space keeps its local branch.

## Other repositories

Agents also find problems that belong to another repository, such as a tool the
project uses. Decision: every command that only talks to the forge names any
repository, registered or not, by `--repo` or by an item's or run's URL. It
needs no space or checkout. A registered checkout is one way to name a
repository, never a requirement. Commands that need a space or checkout, such
as `link`, `watch` and `pr open`, keep their rules.

Every caller reads another repository as it reads its own, since `forge api`
already reads it. Changing it stays the user's, with the user's own logins, and
a scoped caller's change there is refused.

### Issues in other repositories

`issue open --repo` is the exception: it files an issue in another repository,
on any forge, as the agent account. People outside the project see such an
issue, so the `issue_other_repositories` permission governs it apart from
`issue_open`. The workspace's required labels and templates describe its own
repository and do not apply there. The issue cannot be linked, since an
assignment's cleanup follows the workspace's repository. A proposal records the
repository it opens in.

### Registered checkouts

Item commands that need only a repository also run from a registered checkout
outside any space. Only the user acts there, so they act with the user's own
logins under the repository's settings, and open without a proposal. They link
and record nothing, since a checkout holds no links. Workspace processes act
from their workspace.

A chat acts from its repository's checkout too, but as the agent account, under
its own scope. It records what it opens as a workspace does: a new issue is
linked as opened, or assigned with `--link`, and its item actions default to its
linked items. [Chats' links](links.md#chats-links) says how a chat holds them.

An unscoped client such as Quay names a repository with `--repo` instead of
picking a workspace, whose worktree Shoal would check first. A registered
repository's checkout is the place to act in, so it reaches PR, view and CI
commands from anywhere. An item URL of a registered repository selects it
outside a workspace and checkout. Where `--repo` names a registered repository
to read, Shoal reads it through its checkout the same way.
