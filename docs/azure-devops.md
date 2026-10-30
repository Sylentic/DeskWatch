# Azure DevOps source

The `azure_devops` source shows pipeline runs and open pull requests from Azure DevOps Services
(`dev.azure.com`) on the dashboard. It only reads, and only polls: Azure DevOps cannot reach a server at home, so
nothing at home is exposed to the internet.

What you get:

- The latest run of every YAML pipeline on the `pipelines` page, next to Gitea and GitHub.
- A running pipeline shows on the kiosk dashboard with its current stage and task, such as
  `apply: terraform apply`, and a bar that moves with the stages. With `interrupt` on it also shows as the banner on the dashboard. Pipelines whose name contains `deploy` or
  `terraform` show as deploys.
- A failed run raises the usual red alert naming the failed task; a good run flashes green. Both leave a count in
  the header badge until you dismiss it.
- A stage that waits for an approval raises one "Approval needed" notice (another check, such as business hours, "Check pending") and a `review` row on the pipelines page.
  It does not hold the screen, because an approval can take hours.
- Open pull requests count in the PR badge and appear on the `prs` page.
- A refused or expired token, or an unreachable service, shows the **Source down** pill.

Self-hosted agent pools can be shown on the kiosk `runners` widget (optional, needs one more token scope, see
[runners.md](runners.md#4-azure-devops-agent-pools)). Not supported: classic release pipelines, Azure DevOps Server
(on premises). Sign-in as a Microsoft Entra service principal is possible, see [azure-devops-auth.md](azure-devops-auth.md).

This source runs against a real Azure DevOps organisation: runs, stages, approvals and pull requests are checked
there. Agent pools and some check types are only tested against mock answers.
[azure-devops-verify.md](azure-devops-verify.md) is the checklist for trying those on your own organisation.

Only using Azure DevOps? See [Using only one host](git-hosts.md#using-only-one-host-azure-devops-or-github-enterprise).
To sign in as a service principal instead of with a personal access token (not tied to one person), see
[azure-devops-auth.md](azure-devops-auth.md).

## Before you start

A personal access token (PAT) acts as you. If this is your employer's organisation, check that your organisation
allows PATs and that a token on a home server is acceptable. Organisation admins can block PAT creation or limit
how long a token lives. DeskWatch never asks for the token anywhere but its own credential file, and the token
is never written to the config, the logs or MQTT.

## 1. Create a read-only token

1. In Azure DevOps, open **User settings** (top right) > **Personal access tokens** > **New Token**.
2. **Organization**: pick the one organisation to watch. A token for "All accessible organizations" is more than
   DeskWatch needs.
3. **Expiration**: choose a date you will remember. When the token lapses the dashboard shows the **Source down** pill.
4. **Scopes**: choose **Custom defined**, then **Show all scopes**, and tick only:

   | Scope | Access | Used for |
   |---|---|---|
   | **Build** | **Read** | Pipeline runs and their stages and tasks |
   | **Code** | **Read** | Open pull requests (leave out and set `pull_requests = false` if you do not want PRs) |
   | **Agent Pools** | **Read** | Optional: self-hosted agents on the kiosk `runners` widget (`agent_pools`) |

   Nothing else is needed. Do not give **Full access**, and do not tick any Write, Manage or Execute access.
5. Create the token and copy it once; Azure DevOps does not show it again.

The account behind the token also needs to be able to see the project and its pipelines (a member of the project,
or at least Reader on it).

## 2. Store it on the server

These are the Linux commands. On Windows, create the same file (`azdo-work`) in
`C:\ProgramData\DeskWatch\credentials\` as described in [windows.md](windows.md#secrets); the config is the same.

```sh
sudo install -d -m 700 /etc/deskwatch/credentials
sudo sh -c 'umask 077; cat > /etc/deskwatch/credentials/azdo-work'   # paste the token, then Ctrl-D
```

Add the credential to the systemd unit (`sudo systemctl edit deskwatch-bridge`):

```ini
[Service]
LoadCredential=azdo-work:/etc/deskwatch/credentials/azdo-work
```

The file holds the token and nothing else; a trailing newline is fine.

## 3. Add the block to the config

```toml
[[source.azure_devops]]
name = "work"                      # used in logs; letters, digits, - and _
organization = "your-org"          # only the name from https://dev.azure.com/your-org
projects = ["project-a"]           # one or more projects, as shown in Azure DevOps
token_file = "azdo-work"           # the credential name from step 2
```

Then `sudo systemctl restart deskwatch-bridge` and check `journalctl -u deskwatch-bridge` for
`polling Azure DevOps` (on Windows, restart the service and look in its log file).

What to enter, using the browser address `https://dev.azure.com/your-org/My%20Project` as the example:

| Key | Enter | Not |
|---|---|---|
| `organization` | `your-org` | the whole address, or `your-org/My Project` |
| `projects` | `My Project`, with the space (`My%20Project` as the browser shows it also works) | `project-a/...` or a repository name |
| `base_url` (the setup page calls it Address) | leave out, or `https://dev.azure.com` | `https://dev.azure.com/your-org`: the bridge adds the organisation itself, so repeating it ends in 404. The bridge drops a repeated organisation from the address, but keep it clean |

Optional keys:

| Key | Default | Meaning |
|---|---|---|
| `poll_s` | `60` | Seconds between polls of builds, approvals and open PRs |
| `job_poll_s` | `5` | Seconds between step progress polls, only for builds in progress |
| `interrupt` | `false` | `true`, or a list of projects that may take over the screen. Off means work pipelines never show as the banner on the dashboard: they show on the pipelines page, in the badges and on the kiosk dashboard |
| `pull_requests` | `true` | Count open PRs (needs **Code: Read**) |
| `notify_new_prs` | `true` | Flash "New PR" for a new non-draft PR |
| `alert_pr_comments` | `true` | Put a new comment on an open PR in the alerts list until it is dismissed. Your own comments and system messages (votes, pushes) are skipped. One extra request per open PR (at most 30) per poll |
| `deploy_words` | `["deploy", "terraform"]` | Pipelines whose name contains one of these words (any case) are deploys |
| `agent_pools` | none | Names of self-hosted agent pools to show as runners (needs **Agent Pools: Read**) |
| `agent_poll_s` | `60` | Seconds between agent pool polls |
| `alias` | none | Short labels: `"project-a" = "infra"` for pipelines, `"project-a/repo-a" = "A"` for a repository's PRs |

Use `alias` for anything you would rather not show on a screen at the office.

## What the bridge asks for

All requests are `GET` with API version 7.1, authenticated as the token's user. Per project, once per `poll_s`:

| Request | Used for |
|---|---|
| `{project}/_apis/build/builds` (newest 50) | Latest run of each pipeline |
| `{project}/_apis/build/builds/{id}/timeline` | Stage and task progress and approvals of each build in progress; the failed task of a newly failed build |
| `{project}/_apis/git/pullrequests` (active, newest 100) | Open PRs |

With `agent_pools` set it adds one request for the pool list and two per pool (agents and job requests) every `agent_poll_s`. That is two requests per project when nothing runs. While a build runs it adds one timeline request per poll, or
one every `job_poll_s` for every build in progress (the kiosk's Running now widget lists them, whether or not the
project may show as the banner on the dashboard).

## Limits

- Only the 50 most recently queued builds per project are looked at, so a pipeline that last ran before those
  50 does not show.
- Open PRs are capped at 100 per project.
- An approval is recognised from a `Checkpoint.Approval` record in the build timeline. Other checks that gate a
  stage (branch control, business hours, invoked functions) are recognised from any other `Checkpoint.*` record
  that is in progress. They show as "Check pending" and a `review` row, but only when two timeline polls in a
  row (about 10 seconds) saw them, because most checks pass at once. **Both record names are written from the
  documented timeline shape and not yet verified against a real run:** if a check type is named differently the
  build simply keeps showing as running.
- Runs that finished before the bridge started show on the pipelines page and count in the red badge, but do not
  take over the screen.

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| **Source down** pill, journal says the sign-in page was returned or the request was refused | Token expired, revoked, created for a different organisation, or missing the **Build: Read** scope |
| `warn` badge only with `pull_requests = true` | Token lacks **Code: Read**; add it or set `pull_requests = false` |
| **Source down** pill, journal says `404` | Wrong `organization` or project name (spaces are fine, the bridge sends them as `%20`). The test button on the setup page shows the path that was asked for and lists the projects the token can see: copy the name from there exactly |
| Pipeline never appears | It has not run among the newest 50 builds of the project |
| Deploy shows "running" while it waits for an approval or check | The record type is not one the bridge knows. Run with `RUST_LOG=debug`, or look at the build timeline (`.../builds/{id}/timeline`) for the `type` of the record that is `inProgress`, and open an issue |
