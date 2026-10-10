# Azure DevOps source

The `azure_devops` source shows pipeline runs and open pull requests from Azure DevOps Services
(`dev.azure.com`) on the panel. It only reads, and only polls: Azure DevOps cannot reach a server at home, so
nothing at home is exposed to the internet.

What you get:

- The latest run of every YAML pipeline on the `pipelines` page, next to Gitea and GitHub.
- A running pipeline shows on the kiosk dashboard with its current stage and task, such as
  `apply: terraform apply`, and a bar that moves with the stages. With `interrupt` on it also takes over the ESP screen. Pipelines whose name contains `deploy` or
  `terraform` show as deploys.
- A failed run raises the usual red alert naming the failed task; a good run flashes green. Both leave a count in
  the header badge until you press the button.
- A stage that waits for an approval raises one "Approval needed" notice and a `review` row on the pipelines page.
  It does not hold the screen, because an approval can take hours.
- Open pull requests count in the PR badge and appear on the `prs` page.
- A refused or expired token, or an unreachable service, lights the `warn` badge.

Not in this version: agent pools and self-hosted agent status (planned, see [runners.md](runners.md)), classic release pipelines, Azure DevOps Server
(on premises), and sign-in with Microsoft Entra instead of a token.

## Before you start

A personal access token (PAT) acts as you. If this is your employer's organisation, check that your organisation
allows PATs and that a token on a home server is acceptable. Organisation admins can block PAT creation or limit
how long a token lives. DeskWatch never asks for the token anywhere but its own credential file, and the token
is never written to the config, the logs or MQTT.

## 1. Create a read-only token

1. In Azure DevOps, open **User settings** (top right) > **Personal access tokens** > **New Token**.
2. **Organization**: pick the one organisation to watch. A token for "All accessible organizations" is more than
   DeskWatch needs.
3. **Expiration**: choose a date you will remember. When the token lapses the panel shows the `warn` badge.
4. **Scopes**: choose **Custom defined**, then **Show all scopes**, and tick only:

   | Scope | Access | Used for |
   |---|---|---|
   | **Build** | **Read** | Pipeline runs and their stages and tasks |
   | **Code** | **Read** | Open pull requests (leave out and set `pull_requests = false` if you do not want PRs) |

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
organization = "your-org"          # from https://dev.azure.com/your-org
projects = ["project-a"]           # one or more projects
token_file = "azdo-work"           # the credential name from step 2
```

Then `sudo systemctl restart deskwatch-bridge` and check `journalctl -u deskwatch-bridge` for
`polling Azure DevOps` (on Windows, restart the service and look in its log file).

Optional keys:

| Key | Default | Meaning |
|---|---|---|
| `poll_s` | `60` | Seconds between polls of builds, approvals and open PRs |
| `job_poll_s` | `5` | Seconds between step progress polls, only for builds in progress |
| `interrupt` | `false` | `true`, or a list of projects that may take over the screen. Off means work pipelines never take over the ESP screen: they show on the pipelines page, in the badges and on the kiosk dashboard |
| `pull_requests` | `true` | Count open PRs (needs **Code: Read**) |
| `notify_new_prs` | `true` | Flash "New PR" for a new non-draft PR |
| `deploy_words` | `["deploy", "terraform"]` | Pipelines whose name contains one of these words (any case) are deploys |
| `alias` | none | Short panel labels: `"project-a" = "infra"` for pipelines, `"project-a/repo-a" = "A"` for a repository's PRs |

Use `alias` for anything you would rather not show on a screen at the office.

Add the `pipelines` and `prs` pages to `[[rotation]]` if they are not there yet (see `config.example.toml`).

## What the bridge asks for

All requests are `GET` with API version 7.1, authenticated as the token's user. Per project, once per `poll_s`:

| Request | Used for |
|---|---|
| `{project}/_apis/build/builds` (newest 50) | Latest run of each pipeline |
| `{project}/_apis/build/builds/{id}/timeline` | Stage and task progress and approvals of each build in progress; the failed task of a newly failed build |
| `{project}/_apis/git/pullrequests` (active, newest 100) | Open PRs |

That is two requests per project when nothing runs. While a build runs it adds one timeline request per poll, or
one every `job_poll_s` for every build in progress (the kiosk's Running now widget lists them, whether or not the
project may take over the ESP screen).

## Limits

- Only the 50 most recently queued builds per project are looked at, so a pipeline that last ran before those
  50 does not show.
- Open PRs are capped at 100 per project.
- An approval is recognised from a `Checkpoint.Approval` record in the build timeline. Other checks that gate a
  stage (branch control, business hours) are not shown as "Approval needed".
- Runs that finished before the bridge started show on the pipelines page and count in the red badge, but do not
  take over the screen.

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| `warn` badge, journal says the sign-in page was returned or the request was refused | Token expired, revoked, created for a different organisation, or missing the **Build: Read** scope |
| `warn` badge only with `pull_requests = true` | Token lacks **Code: Read**; add it or set `pull_requests = false` |
| `warn` badge, journal says `404` | Wrong `organization` or project name (spaces are fine, the bridge encodes them) |
| Pipeline never appears | It has not run among the newest 50 builds of the project |
| Deploy shows "running" while it waits for an approval | The wait is a kind of check other than an approval |
