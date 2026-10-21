# Bitbucket Cloud source

The `bitbucket` source shows open pull requests and Pipelines runs from Bitbucket Cloud (`bitbucket.org`) on the
panel. It only reads, and only polls: nothing at home is exposed to the internet and no webhook is needed.
Bitbucket Data Center / Server is a different API and is not supported.

What you get:

- Open **pull requests** count as the open PRs: the PR badge, the `prs` page and the kiosk PR column. A new,
  non-draft pull request flashes "New PR" (`notify_new_prs = false` turns that off).
- The latest **pipeline** of each pipeline name on the `pipelines` page, next to the other sources.
- A running pipeline shows on the kiosk dashboard as one "Running now" card with its current **step** and a bar
  that is the share of finished steps. With `interrupt` on it also takes over the ESP screen. A pipeline whose name
  contains `deploy` shows as a deploy.
- A failed pipeline raises the usual red alert naming the failed step; a good one flashes green. Both leave a count
  in the header badge until you press the button. A stopped pipeline is neutral and raises nothing.
- A pipeline paused at a manual step shows as `review` on the pipelines page.
- A refused or expired token, or an unreachable server, lights the `warn` badge.

Self-hosted runners can be shown on the kiosk `runners` widget (optional, needs one more right, see
[runners.md](runners.md#6-bitbucket-runners-in-this-version)).

**What was tested.** Nobody working on DeskWatch uses Bitbucket. The data shapes were taken from Atlassian's
published API description (the OpenAPI file of the Cloud REST API 2.0) and tested against mock answers. Nothing has
been connected to a real workspace yet, so what a real server sends is unconfirmed. In particular:

- the step `name` is not in the published description, so a step without one shows as `step 2`;
- the exact request limit per hour is not looked up here, a `429` shows as "not reachable" and polling backs off;
- whether the pull request list always includes `size` (the exact count); without it the count of the pages read
  is used;
- runners: no busy flag exists in the list, and the meaning of the `ENABLED` status is a guess (shown as idle).

[Check it against a real workspace](#6-check-it-against-a-real-bitbucket) once you have one, and send what differs.

## 1. Create a read-only token

Use an access token that can only read. Bitbucket is phasing out app passwords, so do not use one.

1. In Bitbucket open **Repository settings > Access tokens** (or the project or workspace settings, whichever your
   plan offers). A repository token is the tidiest: it can only see that repository.
2. Give it a name and an expiry date you will remember. When the token lapses the panel shows the `warn` badge.
3. Rights: **Pull requests: Read** and **Pipelines: Read**. Add **Runners: Read** only if you want the `runners`
   widget, and **Issues: Read** for new issues in the alerts list. Nothing with Write or Admin.
4. Create the token and copy it once; Bitbucket does not show it again.

The token is sent as a bearer token. A token only sees the repositories it was made for, and a repository it
cannot see answers like one that does not exist.

## 2. Store it on the server

Store the token as a credential file, like the other sources: `/etc/deskwatch/credentials/bitbucket-personal` (mode
600) with `LoadCredential=bitbucket-personal:/etc/deskwatch/credentials/bitbucket-personal` in the unit, or
`C:\ProgramData\DeskWatch\credentials\bitbucket-personal` on Windows ([windows.md](windows.md#secrets)). In Docker
it is a file in the credentials volume ([docker.md](docker.md)). The Setup page stores it for you if you add the
source there (section **Sources**, "Add a source", Bitbucket). The token never goes into the config, the logs or MQTT.

## 3. Add the block

```toml
[[source.bitbucket]]
name = "personal"
token_file = "bitbucket-personal"
repos = ["your-workspace/repo-a", "your-workspace/repo-b"]
```

| Key | Default | Meaning |
|---|---|---|
| `name` | required | Unique among Bitbucket sources; letters, digits, `-` and `_`. |
| `token_file` | required | Name of the credential holding the token. |
| `repos` | required | `workspace/repo-slug`, both as in the repository address. |
| `base_url` | `https://api.bitbucket.org/2.0` | Only for tests and proxies. Links to pipelines point to `bitbucket.org` only when this is the default. |
| `poll_s` | 120 | Seconds between polls of pull requests and recent pipelines (2 requests per repository). Bitbucket limits requests per hour, so this is slower than the other sources. |
| `job_poll_s` | 10 | Seconds between step polls while a pipeline is in progress. |
| `interrupt` | `true` | May a running pipeline and a finished run take over the ESP screen? `true`, `false` or a list of repositories. |
| `notify_new_prs` | `true` | Flash "New PR" for a new non-draft pull request. |
| `alert_new_issues` | `true` | Put a new issue in the alerts list until it is dismissed. Only for repositories with the issue tracker on, and the token needs **Issues: Read**. One extra request per repository per poll; issues that existed at startup do not alert. |
| `runners` | none | Runner lists for the kiosk `runners` widget. |
| `runner_poll_s` | 300 | Seconds between runner polls. |
| `alias` | none | Short panel labels, such as `{ "your-workspace/repo-a" = "A" }`. |

Press **Test connection** in Setup (or look at the bridge log after a restart): each repository is checked with the
token and the answer says whether the token or the repository name is wrong.

## 4. How a Bitbucket pipeline becomes a row

- **Pipeline name.** Bitbucket pipelines are not tied to a workflow file. A pipeline started from a custom pipeline
  (`custom: deploy-prod`) is called by that name; every other pipeline is called `bitbucket-pipelines.yml`. The
  latest pipeline of each name is shown, whatever branch it ran on.
- **Branch.** A branch or tag shows by its name.
- **Status.** `PENDING` is queued, `IN_PROGRESS` is running, `PAUSED` is waiting (a manual step). A `COMPLETED`
  pipeline is success (`SUCCESSFUL`), failed (`FAILED`, `ERROR`) or neutral (`STOPPED`, anything unknown).
- **Steps.** While a pipeline is in progress its steps are read (one extra request per `job_poll_s`). The card
  shows the running step and the share of steps that are done. A pipeline still being set up has no steps yet and
  shows without a bar.
- **Failing step.** One extra request per failed pipeline, the first time it is seen.
- **Times.** `created_on` and `completed_on` of the pipeline.

## 5. Limits

- No webhooks: a change shows up within `poll_s`, a finished pipeline within a few seconds.
- A rate limit or an unknown repository (`429`, `404`) shows as "not reachable" and polling backs off (2, 3, then 5
  minutes). A refused token (`401`, `403`) shows as "sign-in refused".
- Runners have no busy flag and no link to their page.
- Only the first 5 pages of a list are read (50 pull requests per page, 20 pipelines per request), and a `next`
  link to a different host is never followed.
- Bitbucket Data Center / Server and Bitbucket's own deployment environments are not covered.

## 6. Check it against a real Bitbucket

Once you have a workspace: add one repository, press Test connection, then check that

1. the pull request count matches the repository page,
2. a started pipeline shows as a card with the right step and a sensible bar,
3. a failing step shows its name in the red alert,
4. the pipeline names make sense for your repository (see section 4),
5. if you use runners, the list matches Workspace settings.

If something is off, `deskwatch-bridge` with `RUST_LOG=deskwatch_bridge=debug` logs each poll. Send a sanitised copy
of the answers (with workspace, repository names, ids and commits made up) and the shapes in
`bridge/src/bitbucket/testdata/` can be fixed.
