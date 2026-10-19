# Which Git hosts work

DeskWatch reads open pull requests, CI runs and (optionally) runners from a Git host. Each host type is a
source. This page says what exists, what happens with hosts that look like one of them, and what a new host would
take.

| Host | Source | State |
|---|---|---|
| Gitea | `gitea` | Works: webhooks for instant job start and end, polling for step progress and PRs. Tested on a real Gitea 28. |
| GitHub, GitHub Enterprise Server, GHE.com | `github` | Works by polling. |
| GitLab (gitlab.com, self-managed) | `gitlab` | Built, tested against mock answers only. See [gitlab.md](gitlab.md). |
| Azure DevOps Services | `azure_devops` | Built, tested against mock answers and a first connection. See [azure-devops.md](azure-devops.md). |
| Forgejo, Codeberg | (`gitea`, in part) | Not tested, see below. |
| Bitbucket Cloud, Bitbucket Data Center | none | Not built, see below. |
| Gogs, Gitee, SourceHut, others | none | Not planned. |

## Forgejo and Codeberg

Forgejo is a fork of Gitea, and Codeberg runs Forgejo. Nobody has tried DeskWatch against one, so this is what the
code and the Forgejo documentation suggest, not what was seen.

Likely to work with a plain `[[source.gitea]]` block:

- **The webhook.** Forgejo sends its own `X-Forgejo-*` headers and also the `X-Gitea-*` ones with the same values,
  including the HMAC-SHA256 signature in `X-Gitea-Signature`, which is what the bridge checks
  ([Forgejo webhook documentation](https://forgejo.org/docs/latest/user/webhooks/)).
- **Open PRs.** The bridge polls `GET /api/v1/repos/{owner}/{repo}/pulls` and reads the total from `X-Total-Count`.
  Forgejo keeps Gitea's API for this.
- **Test connection** in Setup (`/version`, `/user`, `/repos/{owner}/{repo}`).

Unlikely to work, unconfirmed:

- **CI runs and the running-job card.** The Gitea source starts from the `workflow_run` and `workflow_job` webhook
  events and then polls `GET /repos/{owner}/{repo}/actions/jobs/{id}`. As far as I know Forgejo Actions does not send
  those two events (it has its own action run events) and may not have that endpoint, so the pipelines page, the
  running card and the red and green alerts would stay empty. I could not confirm either point without a Forgejo
  instance.
- **Runner lists.** They use Gitea's `.../actions/runners` endpoints, which Forgejo may not offer.

So today a Forgejo or Codeberg repository would give the PR count and nothing else. Making the CI part work would
be a `forgejo` source that polls Forgejo's own run list (`GET /repos/{owner}/{repo}/actions/runs`, which Forgejo has
been adding to its API) the way the GitHub and GitLab sources poll. That needs a Forgejo to read real answers from, so
it waits until you have one. If you do, try the `gitea` block first, add one project, and tell me what shows.

Woodpecker CI and Drone, which are often used next to Forgejo, are separate CI servers and are not supported.

## Bitbucket

Not built. What it would take, from the Bitbucket documentation and my memory of it (I could not check the current
pages from here, so treat the details as a starting point):

- **Bitbucket Cloud** (`bitbucket.org`), REST API 2.0 at `https://api.bitbucket.org/2.0`:
  - open pull requests: `GET /repositories/{workspace}/{repo}/pullrequests?state=OPEN`, paged with `next` links,
    `size` gives the count;
  - Bitbucket Pipelines: `GET /repositories/{workspace}/{repo}/pipelines/?sort=-created_on`, each with a state
    (`PENDING`, `IN_PROGRESS`, `COMPLETED`) and a result (`SUCCESSFUL`, `FAILED`, `STOPPED`, ...), and the steps of a
    pipeline under `/pipelines/{uuid}/steps/`, which gives a real step progress bar;
  - runners (workspace or repository self-hosted runners) have their own API and would be a later addition;
  - authentication: a repository or workspace access token sent as a bearer token, read-only scopes for pull
    requests and pipelines. App passwords are being phased out by Atlassian, so a new source should not rely on them.
- **Bitbucket Data Center / Server** is a different API (`/rest/api/1.0/...`) and a different CI story (build
  statuses from Bamboo, Jenkins or others). It would be a separate source, and is the less likely one to be wanted.

Cloud would be about the size of the GitLab source: an API client, payload shapes, a state machine that turns
pipelines and steps into the shared run and job facts, a Setup form, a connection test, mocks and a docs page. The
main unknowns are the exact pagination of the pipelines list and what the step states look like in a running
pipeline, which want a real Bitbucket workspace to confirm. Tell me if you want it and which of the two it is.

## Adding a host in general

Every source ends in the same facts (open PRs, the latest run per pipeline, running jobs, runners), so the page,
the kiosk and the ESP screen do not change. A new host is one module under `bridge/src/<host>/` (API client,
payload types, a state machine, tests with fixtures), one config struct in `config.rs`, a line each in the
supervisor, the Setup page and the connection test, and a docs page. `bridge/src/gitlab/` is the most recent
example to copy.
