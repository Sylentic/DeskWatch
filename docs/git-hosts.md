# Which Git hosts work

DeskWatch reads open pull requests, CI runs and (optionally) runners from a Git host. Each host type is a
source. This page says what exists, what happens with hosts that look like one of them, and what a new host would
take.

| Host | Source | State |
|---|---|---|
| Gitea | `gitea` | Works: webhooks for instant job start and end, polling for step progress and PRs. Tested on a real Gitea server. |
| GitHub, GitHub Enterprise Server, GHE.com | `github` | Works by polling. Tested on real repositories. |
| GitLab (gitlab.com, self-managed) | `gitlab` | Built, tested against mock answers only. See [gitlab.md](gitlab.md). |
| Azure DevOps Services | `azure_devops` | Works by polling. Tested on a real organisation. See [azure-devops.md](azure-devops.md). |
| Forgejo, Codeberg | (`gitea`, in part) | Not tested, see below. |
| Bitbucket Cloud | `bitbucket` | Built, tested against mock answers only. See [bitbucket.md](bitbucket.md). |
| Bitbucket Data Center / Server | none | Not built, see below. |
| Gogs, Gitee, SourceHut, others | none | Not planned. |

## What each host adds to the alerts list

Besides runs and pull requests, a source can put activity in the Alerts list as info alerts (blue, with the source's mark,
linking to the item, kept until dismissed or for a week). Items that existed at startup do not alert.

| Source | New issues (`alert_new_issues`) | New PR comments (`alert_pr_comments`) |
|---|---|---|
| Gitea | Yes, from the **Issues** webhook event | No |
| GitHub, GitHub Enterprise | Yes, polled (token needs **Issues: Read**) | No |
| GitLab | Yes, polled | No |
| Bitbucket Cloud | Yes, polled (issue tracker on, token needs **Issues: Read**) | No |
| Azure DevOps | No | Yes, polled (your own comments are skipped) |

Both are on by default and can be turned off per source. New issues and PR comments are tested against mock answers only.

## Using only one host (Azure DevOps or GitHub Enterprise)

An organisation that only uses Azure DevOps, or only GitHub Enterprise, needs none of the others: no Gitea, no
Prometheus. Each source is its own block, so unused ones are simply not configured. A bridge with one
source and the dashboard looks like this (see [docker.md](docker.md) for the container side, [setup-page.md](setup-page.md)
to fill it in from the browser instead):

```toml
[mqtt]
enabled = false          # no broker, no warnings about one

[kiosk]
enabled = true

# Azure DevOps ...
[[source.azure_devops]]
name = "work"
organization = "your-org"
projects = ["project-a"]
token_file = "azdo-work"     # or a service principal, see azure-devops-auth.md

# ... or GitHub Enterprise Server (use `https://api.<subdomain>.ghe.com` for GHE.com)
# [[source.github]]
# name = "work"
# base_url = "https://ghe.example.com/api/v3"
# token_file = "ghe-work"
# repos = ["org/repo-a"]
```

Several at once also works: add one block per host, and the dashboard and the badges merge them (the `source`
filter of a `[[kiosk.panel]]` shows one host only). The Hosts panel only shows machines that a Prometheus source
reports, and the `runners` widget shows self-hosted runners and agents of every source that has them listed.

## Signing in without a person's token

A personal token acts as one person and stops when they leave or lose access. What each source offers instead:

| Source | Not tied to a person | In DeskWatch |
|---|---|---|
| Azure DevOps | Entra service principal (Basic licence per identity, two admins). [azure-devops-auth.md](azure-devops-auth.md) | **Built**: `auth = "entra"` |
| GitLab | Project or group access token: a bot user with its own role (Reporter is enough), no licence on most plans | Works today, it is a token like any other. [gitlab.md](gitlab.md) |
| Bitbucket Cloud | Repository, project or workspace access token (bearer). App passwords stopped working on 9 June 2026; API tokens are tied to a user | Works today, same token field. [bitbucket.md](bitbucket.md) |
| GitHub, GitHub Enterprise | A GitHub App installed on the organisation (an hour-long installation token made from the app's private key), or a fine-grained token on a dedicated machine user | **Built**: `auth = "app"`, see below |
| Gitea | A token of a dedicated bot user | Works today |

GitLab, Bitbucket and Gitea already have the "service account" option, as a token you create on the bot or on the
project, so they need no code. GitHub and Azure DevOps are the two where the token is easily a person's, which is why
Azure DevOps got a real alternative.

### GitHub App

Create the App under the organisation's **Settings > Developer settings > GitHub Apps** with read-only rights:
Metadata, Pull requests, Actions, Issues (for new-issue alerts), plus Administration (repository runners) or
Self-hosted runners (organisation runners) if you list `runners`. Install it on the repositories to watch, generate a
private key (a `.pem` file) and note the **App ID** and the **installation id** (the number at the end of the
installation's address). Then, in Setup, add a GitHub source and choose **Sign in with: GitHub App**, or in a config file:

```toml
[[source.github]]
name = "work"
auth = "app"
app_id = "123456"
installation_id = "7890123"
private_key_file = "github-work-key"   # credential holding the .pem text
repos = ["team/service-a"]
```

The bridge signs a short token with the key, swaps it for an installation token that lasts an hour, renews it before
it runs out and gets a new one if GitHub revokes it. The key is stored like a token (the data folder's `credentials/`,
never `sources.toml`) and never comes back to the browser. **This has been tried against mock answers only**, not
against a real GitHub App. A key GitHub gave out as `BEGIN RSA PRIVATE KEY` or `BEGIN PRIVATE KEY` both work.

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
  events and then polls `GET /repos/{owner}/{repo}/actions/jobs/{id}`. As far as the documentation shows, Forgejo Actions does not send
  those two events (it has its own action run events) and may not have that endpoint, so the pipelines page, the
  running card and the red and green alerts would stay empty. Neither point is confirmed without a Forgejo
  instance.
- **Runner lists.** They use Gitea's `.../actions/runners` endpoints, which Forgejo may not offer.

So today a Forgejo or Codeberg repository would give the PR count and nothing else. Making the CI part work would
be a `forgejo` source that polls Forgejo's own run list (`GET /repos/{owner}/{repo}/actions/runs`, which Forgejo has
been adding to its API) the way the GitHub and GitLab sources poll. That needs a Forgejo to read real answers from, so
it waits until someone has one. If you do, try the `gitea` block first, add one project, and
[tell us what shows](https://github.com/Sylentic/DeskWatch/issues/new/choose).

Woodpecker CI and Drone, which are often used next to Forgejo, are separate CI servers and are not supported.

## Bitbucket

Bitbucket Cloud (`bitbucket.org`) has its own source, see [bitbucket.md](bitbucket.md). It was written from
Atlassian's published API description and tested against mock answers only, because nobody on the project uses
Bitbucket. Feedback from a real workspace is what turns it from "built" into "works".

**Bitbucket Data Center / Server** is a different API (`/rest/api/1.0/...`) and a different CI story (build
statuses from Bamboo, Jenkins or others). It would be a separate source and is not planned.

## Adding a host in general

Every source ends in the same facts (open PRs, the latest run per pipeline, running jobs, runners), so the dashboard
does not change. A new host is one module under `bridge/src/<host>/` (API client,
payload types, a state machine, tests with fixtures), one config struct in `config.rs`, a line each in the
supervisor, the Setup page and the connection test, and a docs page. `bridge/src/bitbucket/` is the most recent
example to copy.
