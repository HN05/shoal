# Forges

This file covers `shoal-forge`: forge accounts, item references, forge reads and
writes, branch deletion and other repositories.

Forge changes Shoal makes for a workspace act as the agent too, with the host's
configured `agent_token`, and so does the Git of agents and of the pushes Shoal
runs for them over HTTPS to that host, through a credential helper the agent's
Git settings scope to the host after clearing the user's helpers for it, so the
token never enters a file, the environment or argv, and that Git asks before its
first request, ahead of a login of the user's `.netrc` for the host.

An issue, PR or CI run is read once, where it enters, into an item reference
of its repository, kind and number: a command's argument, a protocol field, a
stored link as it loads, or a forge's answer. Code past that point passes the
reference and never reads its text again. Stores key items by their canonical
URL text, read back into a reference as each row loads. A number names an item
only once the repository it belongs to is known.

Shoal owns forge writes for its workspaces so that every client, from an agent
in a terminal to an app built on Shoal, gets one item model, one account rule
and one policy. Issue and PR actions are typed (edit, comment, close, reopen,
merge) and planned per forge as REST requests, sent with curl and the token
`[machine.forges."<host>"]` gives the host; without one only reads go out,
anonymously. GitHub's GraphQL requests go the same way. Shoal needs only a token per host, so it runs
no forge CLI and reads none of their login files, and a forge works without its
CLI installed. A token
is given in the shared value form, so it may come from a command such as a
password manager, on this machine or another of its hub, and is fetched at
most once per operation and never stored;
an `{ env }` token reads the environment of the process sending the request,
which for the daemon is the service's. The agent account has its own
`agent_token` per host; without one it reads with the user's token, as the
agent itself would, but changes nothing, so no change goes out as the user
unless `agent_token = { same_as = "user_token" }` says so. A change to a host
with no token for the account is refused before it is sent, naming the setting
and the reload the daemon needs, since a forge answers an unsigned change only
with a bare 401.
The forge is selected from the remote's host without requiring configuration:
well-known hosts by name, and any other by asking it once through each forge's
version or metadata endpoint with the user's token for that host, since a name
cannot tell a GitHub Enterprise Server or a self-hosted GitLab from Forgejo;
the token goes only to the host it is set for. The daemon records
each answer in its state and asks recorded hosts again when it starts or
reloads its config, recording a changed answer with an alert. A host it cannot reach then
keeps its answer and raises no alert, since a machine may start offline; a global `[machine.forges."<host>"] type`
pins one instead and is never asked, and a host that answers as none fails with a message naming that
setting. Each process remembers the answers it resolved, so links of the host
parse the same way, and the CLI starts out knowing the pinned ones. A pasted
link of a host not yet resolved is classified without a request by the forge
its routes name, such as `/pull/` for GitHub
and `/-/merge_requests/` for GitLab; only an issue or repository URL, whose
form several forges share, falls back to the name: GitLab for a `gitlab` name
label, and otherwise Forgejo. Host types describe hosts rather than
repositories, so they are global only, like the tokens in the same table. GitLab-only concepts such as approval rules and merge trains
stay unmodeled; approvals count as approving reviews. Forge differences stay in planning, such as Forgejo keeping draft
state in the title, so callers see the same item either way. Milestones are
named by title and looked up as the ID each forge takes, and a new item gets its
assignees and milestone from the edit that finishes it, so one path sets them.
An edit fails when the item the forge then reports lacks its labels, assignees
or milestone, because forges drop what they cannot apply without an error. Each backend
declares the capabilities it has, and a request needing one it lacks is refused
before anything is sent with one typed error naming the forge and the
capability, so a backend gains a capability without changes elsewhere;
`shoal forge capabilities` reports them so agents can check first. Where a
forge's releases differ in a capability, the backend asks the server for its
version before the request, so an older server refuses as clearly and the
report matches the server. A revert uses
the forge's own where it has one, and elsewhere reverts the merge with Git in a
new workspace that opens the PR and goes once it merges, so backing out a merge
needs no agent; a conflicting revert does, so it is abandoned rather than left
half done. Which commits the merge added is established from their trees and
patches, never assumed, since reverting a guess can undo someone else's work. Actions run in the
daemon, where scope applies: they act as the workspace's agent account, except
the user's own login among an issue's assignees, while
merging and a review's verdict stay the user's
decisions unless a permission gives them to a scoped caller: `merge` its
merges, as the agent account, and `pr_review` its
verdicts, as the host's `review_token`, a login of its own, since agents open
their PRs as the agent account and forges take no verdict from a PR's
author; the user's own act as the user's login.
A PR merge, now
or automatic, names the head commit a scoped request's approval bound, else the
one Shoal read before merging, so the forge refuses a head pushed since instead
of merging a commit nobody approved; Bitbucket takes none. A forge checks that
head when an automatic merge is set, and only GitLab keeps checking it until
the merge, so `auto_merge_at_head` names the forges where a later push still
merges. A forge that refuses the user's verdict, such as on its author's
own PR, says so itself rather than Shoal guessing first; a scoped verdict is
refused before any request when `review_token` is missing or its account
opened the PR, so the refusal names the setting to change. A review's line
comments are checked against the PR's diff before anything is sent, so a
review is refused whole rather than posted in part. GitLab publishes every
draft note the account has on a merge request at once, so a review there is
refused while other drafts are pending, rather than publishing them with it. Review
threads are named by any of their comments' IDs. Reactions use GitHub's names
on every forge, so a client offers one set, which `shoal forge reactions` narrows
to what an instance allows, and are read for a view with its
comments, which costs a request per comment on forges that list them per comment;
reacting reads first and changes only a reaction not already as asked, since
forges differ in whether adding one twice fails. A comment edit changes only
a comment the acting login wrote, checked against the item's discussion before
anything is sent, since an account with admin rights could otherwise rewrite
someone else's words. Every forge read also runs in the daemon: listing issues
and PRs, items' views, the kind of a bare number, an issue's details and
blockers, a PR's head, possible reviewers and review threads, and a
repository's labels, reactions and capabilities. Apps read them without
choosing a login or a host's forge themselves, and the CLI resolves no forge
host of its own: a scoped caller reads as the agent account and anyone else as
the user's own logins, in another repository too, since checking for a
duplicate before filing there is only a read. A search uses each forge's own search and
reads pages only until the limit is met. A search for words matches them both
ways by default: GitHub's search by meaning misses exact matches, such as an
error message or a flag, that keyword search finds, so `both` adds its matches
to the keyword ones, and gives the keyword ones alone when that search fails,
such as under the stricter rate limit GitHub keeps for it or on a server
without it, so the default never fails a search keyword matching answers. A forge's rate limit for some requests, such as
GitHub's for searches, holds back only those requests, never the watches and
reads the others serve; a pause the forge puts on the whole account, such as a
`Retry-After`, holds back every request. `shoal forge api` passes any request under
the repository's forge API root through the daemon the same way, for reads no
command models. Forgejo's Actions lists ignore `limit` without `page` and
return every run, so a GET there that sets only `limit` reads the first page. The daemon
cannot tell what another method changes, so a scoped caller sends only GET
unless its `api_writes` permission accepts that its writes may merge, approve or skip
required labels, or `"propose"` asks for each write, bound to its method, path
and body, and only GET in another repository, which that setting does
not speak for; the path stays under the API root, so the login reaches only
the API of the forge its repository is on. `shoal forge download` reads a file
such as an image in a description the same way, so a client without forge
logins shows a private repository's images; it takes only URLs on the
repository's forge host, and the daemon sends the bytes back in frames, since
the caller, not the daemon, chooses where they go. GitLab's web routes for
uploads take no access token, so its backend maps an upload URL to the API
route for the same file and reads it with the host's token, like every other
GitLab request; curl sends the token only to that host, not to object storage
a redirect leads to. Bitbucket's files need a browser session, so it
has no downloads. Forgejo's issue search lists PRs
without their branches, so each listed PR is read in full, and GitLab keeps
merged merge requests apart from closed ones, so listing closed ones reads all
and drops the open ones. Shoal does not sign in to a
forge's web UI, so it cannot resolve threads on Forgejo, whose API has no
endpoint for it. Resolving a thread nobody has answered needs confirmation,
because the reply tells the reviewer why. Attached files are read
by the daemon from paths the CLI checked, anywhere on disk: a body file can
already come from anywhere and scope is cooperative, while recordings kept in the
worktree would be untracked files that hold up cleanup. GitHub's REST API has no
upload endpoint, so Shoal uses the one gh itself uses on GitHub's upload host. Forgejo
takes a comment's files only once it is posted, so a failure there would leave it
without them: Shoal refuses what the instance's attachment settings refuse before
posting, detecting a file's media type from its leading bytes as Forgejo does, and resends the uploads and the comment's update after failures that may
pass. An upload is resent only when the forge did not get it or answered 503,
since a second copy would stay on the comment and a proxy's other 5xx may follow a
stored one; the update also after those. A comment whose files still fail stays posted, neither deleted
nor finished later, and its error says so. Each action
returns the item as the forge reports it afterwards and records an
`item_changed` event for integrations. A new issue is recorded as opened and
assigned only on request: a workspace's assigned issues are its work, while an issue
an agent opens is usually follow-up work. Browsing across repositories and review
tooling stay with clients.
An assigned issue shows on the forge who works it through its assignees, since
every forge has them and a label would need one per repository. The agent
account is assigned while an agent works the issue: a space linking it as
assigned has an agent of its own that runs, is queued, restarts or is asleep, or
an agent made the link and no agent of the space's own has ended since, as for a
child a coordinator creates for its subagents, and the space's work is not done. It comes off an open issue once none does, at
`shoal done`, a stop that waits for `shoal resume`, an exit, an unlink or a
removal, and goes back on when an agent starts or is restored or `done --undo`
withdraws the completion. Every Shoal acting as the same account shares that
assignee, so each records the issues it put or kept the account on and takes it
off only those, leaving one another Shoal's agent may work; it reads the forge
for whether the account is there, and settles it in the background, so a slow
forge holds back no start or stop. Once the background poll sees an issue lose
an assignee, the spaces linking it settle the account again, so one another
Shoal took off an issue an agent here works goes back on. The user's own login,
under `[issue] assign = "user"` or `"both"`, joins on every assigned link, whoever makes it, and
leaves through the link and unlink paths, removal among them: Shoal records each
login it added, per issue, so it takes back only its own and only once no space
links the issue for it, never one the user assigned themselves. A closed issue
keeps its assignees. A space's links carry whether an agent assigned each issue
and the user's login Shoal added, so a restore or a move assigns as before and
keeps Shoal's ownership, and a moved space leaves the agent account to the
machine it moved to. Assigning never refuses or undoes the link or the
transition: other assignees and forge failures come back as warnings or
alerts.

`shoal pr open` pushes to the branch's push remote, then finds or opens the branch's
PR and assigns it in one daemon request; a push remote on the origin's forge host
other than `origin` is a fork the PR comes from, so a PR Shoal opens is left unlinked only when removal started
after its check, which the result reports as a warning, or as opened when it
created the PR and assigning it failed. A new PR is ready
unless `--draft` or `[pr] draft` asks for one, because opening a ready PR is
what requests review.
An explicitly supplied description is preserved verbatim; when no description
is supplied, each assigned issue is referenced with `Closes #<number>`.
Repository conventions reach agents where they open and mark items, not only in
instructions read at launch: `shoal repo labels` serves the forge's label scheme with
its descriptions, repositories may require label prefixes, which refuse a new item
before it exists, and missing labels or template headings come back as warnings
that never fail a change that already happened. Shoal never fills a template in,
because an unfilled template says less than a closing reference.
Its options are modeled rather than passed through to a CLI, so they mean the
same on every forge. Label and reviewer choices are listed under the names
those options take, a team as `owner/team`, so a client can offer them without
forge API calls of its own.

Deleting branches on the forge is `shoal branch delete`, which the daemon runs
for a chat as for a workspace, since a chat has no `git push --delete`; `push`
governs it as it governs a pushed deletion, with one `"propose"` request per
branch bound to the commit the branch points to. The daemon deletes each with a
Git push over HTTPS as the caller's account, leased at that commit, since the
forges' APIs delete a branch by name: the forge keeps a branch that a push moved
after the checks. Its token reaches Git on standard input, held by Git's
credential cache for that host alone, never in a file, the environment or argv.
It checks every named branch before deleting any, and refuses the default and
protected branches, the head or base of an open PR, a branch whose changes the
default branch lacks unless `--force`, and an active space's branch outside the
caller's scope whatever `--force` says; a space's branch is active while the
space exists without a recorded completion and the default branch lacks its
changes, and a finished space's branch goes as any other. The space keeps its
local branch.

Agents also find problems that belong to another repository, such as a tool the
project uses, so `issue open --repo` files an issue there, on any forge, as the
agent account. Decision: every command that only talks to the forge names any
repository, registered or not, by `--repo` or an item's or run's URL, which
needs no space or checkout; a registered checkout is one way to name a
repository, never a requirement. Every caller reads another repository as it
reads its own, since `forge api` already reads it, while changing it stays the
user's, with the user's own logins, and a scoped caller's change there is
refused. Commands that need a space or checkout, such as `link`, `watch` and `pr
open`, keep their rules. Item commands that need only a repository also run from
a registered checkout outside any space: they act as the repository's agent
account under its settings, open without a proposal, and link or record nothing,
since a checkout holds no links; workspace processes act from their workspace. A
chat acts the same way from its repository's checkout, under its own scope, but
records what it opens as a workspace does: a new issue is linked as opened, or
assigned with `--link`, and its item actions default to its linked items. A chat
holds links as a workspace does: the link tables name the space, a workspace or
chat, with IDs from their shared namespace, and go with it. Its assigned items
neither complete a chat nor clean it up: only its own `done` records completion,
and a watched PR keeps a chat from idle cleanup but not a done one from
completion cleanup, since a chat has no branch whose work an unmerged PR would
keep; and the activity Shoal polls for them waits on the chat's own cursor,
which the chat's hooks deliver with its messages, waking its agent; a watch in
the chat and those hooks share one delivery, so the newest takes what is
pending. An unscoped client such as Quay names that repository with `--repo`
instead of picking a workspace, whose worktree Shoal would check first: a
registered repository's checkout is the place to act in, so it reaches PR, view
and CI commands from anywhere, and an item URL of a registered repository
selects it outside a workspace and checkout. Where `--repo` already names any
repository to read, a registered one is read through its checkout the same way.
People outside the project see such an issue, so the `issue_other_repositories`
permission governs scoped callers separately from `issue_open`: `"propose"` by
default, `"direct"`, or `"deny"`. The workspace's required labels and templates
describe its own repository and do not apply there, and the issue cannot be
linked, since an assignment's cleanup follows the workspace's repository. A
proposal records the repository it opens in.
