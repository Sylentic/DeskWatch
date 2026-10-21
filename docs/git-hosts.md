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
| Bitbucket Cloud | `bitbucket` | Built, tested against mock answers only. See [bitbucket.md](bitbucket.md). |
| Bitbucket Data Center / Server | none | Not built, see below. |
| Gogs, Gitee, SourceHut, others | none | Not planned. |

## Using only one host (Azure DevOps or GitHub Enterprise)

You do not need Gitea, MQTT, Prometheus or an ESP panel. A bridge that only reads one Git host needs these blocks and
nothing else (see [docker.md](docker.md) for the container side, [setup-page.md](setup-page.md) to fill it in from the
browser instead):

```toml
[mqtt]
enabled = false          # no broker, no warnings about one

[server]
enabled = false          # do not show the machine the bridge runs on (a container id, a work PC)

[kiosk]
enabled = true

# Azure DevOps ...
[[source.azure_devops]]
name = "work"
organization = "your-org"
projects = ["project-a"]
token_file = "azdo-work"

# ... or GitHub Enterprise Server (use `https://api.<subdomain>.ghe.com` for GHE.com)
# [[source.github]]
# name = "work"
# base_url = "https://ghe.example.com/api/v3"
# token_file = "ghe-work"
# repos = ["org/repo-a"]
```

What changes when no host can be shown (`[server] enabled = false` and no Prometheus source):

- The built-in dashboard layout leaves out the empty Hosts panel: Running now and Pipelines on top, Open PRs and
  Alerts below. Panels you place with `[[kiosk.panel]]` are never changed. The layout is read at start, so restart
  the bridge after adding a Prometheus source from Setup to get the Hosts panel back.
- Without `[[rotation]]` blocks the ESP rotation is the pages that exist: `stats` while the server source is on, plus
  `prs` and `pipelines` (skipped while empty) as soon as any Git host is configured. A bridge without a Git host and
  without `[server] enabled = false` rotates the stats page only, as before.
- The Gitea webhook route is only opened for Gitea sources; nothing else listens.

Azure DevOps today needs a personal access token. [azure-devops-auth.md](azure-devops-auth.md) compares the
alternatives (service principal, managed identity, Entra OAuth) and recommends one.

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

Bitbucket Cloud (`bitbucket.org`) has its own source, see [bitbucket.md](bitbucket.md). It was written from
Atlassian's published API description and tested against mock answers only, because nobody on the project uses
Bitbucket. Feedback from a real workspace is what turns it from "built" into "works".

**Bitbucket Data Center / Server** is a different API (`/rest/api/1.0/...`) and a different CI story (build
statuses from Bamboo, Jenkins or others). It would be a separate source and is not planned.

## Adding a host in general

Every source ends in the same facts (open PRs, the latest run per pipeline, running jobs, runners), so the page,
the kiosk and the ESP screen do not change. A new host is one module under `bridge/src/<host>/` (API client,
payload types, a state machine, tests with fixtures), one config struct in `config.rs`, a line each in the
supervisor, the Setup page and the connection test, and a docs page. `bridge/src/bitbucket/` is the most recent
example to copy.
