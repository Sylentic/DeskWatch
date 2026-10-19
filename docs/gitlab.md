# GitLab source

The `gitlab` source shows open merge requests and pipelines from gitlab.com or a self-managed GitLab on the panel.
It only reads, and only polls: nothing at home is exposed to the internet and no webhook is needed.

What you get:

- Open **merge requests** count as the open PRs: the PR badge, the `prs` page and the kiosk PR column. A new,
  non-draft merge request flashes "New PR" (`notify_new_prs = false` turns that off). The count is exact even above
  100, because GitLab sends the total in an `X-Total` header (it leaves it out above 10,000 merge requests, then
  100 is shown).
- The latest **pipeline** of each pipeline name on the `pipelines` page, next to Gitea, GitHub and Azure DevOps.
- A running pipeline shows on the kiosk dashboard as one "Running now" card, however many jobs run side by side.
  GitLab jobs have no steps, so the card counts **stages**: the step reads `test` (or `test: integration` when one
  job of the stage runs) and the bar is the share of finished stages. With `interrupt` on it also takes over the
  ESP screen. A pipeline or job whose name contains `deploy` shows as a deploy.
- A failed pipeline raises the usual red alert naming the first failed job that is not allowed to fail; a good one
  flashes green. Both leave a count in the header badge until you press the button.
- A pipeline stopped at a manual gate (`manual`) shows as `review` on the pipelines page. It raises no notice yet.
- A refused or expired token, or an unreachable server, lights the `warn` badge.

Self-hosted runners can be shown on the kiosk `runners` widget (optional, needs more rights, see
[runners.md](runners.md#5-gitlab-runners-in-this-version)).

**What was tested.** The data shapes were written from the GitLab API documentation and tested against mock
answers. Nobody has connected this source to a real GitLab yet, so what a real server sends is unconfirmed. In
particular the pipeline `name` field, the `X-Total` header and the runner endpoints are as documented, not as
seen. [Check it against a real GitLab](#6-check-it-against-a-real-gitlab) once you have one.

## 1. Create a read-only token

Use a token that can only read.

1. In GitLab open **Preferences > Access tokens** (a personal token), or **Settings > Access tokens** of a
   project or group (a project or group token acts as its own bot user, which is tidier on a shared server).
2. Give it a name and an expiry date you will remember. When the token lapses the panel shows the `warn` badge.
3. Role (project and group tokens): **Reporter** is enough to read merge requests and pipelines.
4. Scope: tick only **`read_api`**. Do not tick `api`, `write_repository` or any other scope.
5. Create the token and copy it once; GitLab does not show it again.

The user behind the token must be able to see each project you list. Runner lists need more, see
[runners.md](runners.md#5-gitlab-runners-in-this-version). Leave `runners` out if you do not need them.

## 2. Store it on the server

Store the token as a credential file, like the other sources: `/etc/deskwatch/credentials/gitlab-personal` (mode
600) with `LoadCredential=gitlab-personal:/etc/deskwatch/credentials/gitlab-personal` in the unit, or
`C:\ProgramData\DeskWatch\credentials\gitlab-personal` on Windows ([windows.md](windows.md#secrets)). In Docker it
is a file in the credentials volume ([docker.md](docker.md)). The Setup page stores it for you if you add the
source there (section **Sources**, "Add a source", GitLab). The token never goes into the config, the logs or MQTT.

## 3. Add the block

```toml
[[source.gitlab]]
name = "personal"
token_file = "gitlab-personal"
projects = ["your-group/project-a", "your-group/subgroup/project-b"]
```

| Key | Default | Meaning |
|---|---|---|
| `name` | required | Unique among GitLab sources; letters, digits, `-` and `_`. |
| `token_file` | required | Name of the credential holding the token. |
| `projects` | required | Project paths, `group/project` or `group/subgroup/project`. The path from the project's address, not its display name. |
| `base_url` | `https://gitlab.com` | The site. Self-managed: `https://gitlab.example.com`, with a sub path if GitLab runs under one. `/api/v4` is added for you (and accepted when you write it). |
| `poll_s` | 60 | Seconds between polls of merge requests and recent pipelines (2 requests per project). |
| `job_poll_s` | 5 | Seconds between job polls while a pipeline is running. |
| `interrupt` | `true` | May a running pipeline and a finished run take over the ESP screen? `true`, `false` or a list of projects. |
| `notify_new_prs` | `true` | Flash "New PR" for a new non-draft merge request. |
| `runners` | none | Runner lists for the kiosk `runners` widget. |
| `runner_poll_s` | 300 | Seconds between runner polls. |
| `alias` | none | Short panel labels, such as `{ "your-group/project-a" = "A" }`. |

Self-managed example:

```toml
[[source.gitlab]]
name = "work"
base_url = "https://gitlab.example.com"
token_file = "gitlab-work"
projects = ["team/service-a"]
interrupt = false        # work pipelines only in the badge and pipelines page
```

Press **Test connection** in Setup (or look at the bridge log after a restart): each project is checked with the
token and the answer says whether the token or the project path is wrong.

## 4. How a GitLab pipeline becomes a row

- **Pipeline name.** GitLab pipelines are not tied to a workflow file. If the project names its pipelines
  (`workflow: name:`, recent GitLab versions) that name is used; otherwise the pipeline is called
  `.gitlab-ci.yml`. The latest pipeline of each name is shown, whatever branch it ran on, which is also how the
  GitHub source works.
- **Branch.** A branch or tag shows by its name. A merge request pipeline shows as `!42`.
- **Status.** `running` is running; `created`, `pending`, `preparing`, `scheduled` and `waiting_for_resource` are
  queued; `success` and `success-with-warnings` are success; `failed` is failed; `manual` is waiting;
  `canceled`, `canceling`, `skipped` and anything unknown are neutral and never raise an alert.
- **Times.** The pipeline list has no finish time, so a finished pipeline uses its last update time. A running
  card uses the earliest job start time.
- **Stage order.** Stages are ordered by the first job created in each stage. With `needs:` (a DAG) jobs of a
  later stage can start early; the card then shows the stage with a running job.
- **Retries.** Only the latest attempt of each job counts.
- **Failing job.** One extra request per failed pipeline, the first time it is seen.

## 5. Limits

- No webhooks: a change shows up within `poll_s` (a finished pipeline within a few seconds, because the last
  finished job triggers a poll).
- GitLab.com allows 2,000 authenticated API calls a minute per user; this source uses 2 per project per minute
  plus the job polls of running pipelines. A rate limit answer (`429`) shows as "not reachable" and polling backs
  off (1, 2, then 5 minutes).
- A project with Pipelines switched off answers `403` and shows as "token refused".
- Group-level runners, child pipelines and downstream (bridge) pipelines are not followed.
- A private CA on a self-managed GitLab is untested. If the test says the certificate is not trusted, put the CA
  in the system store of the machine that runs the bridge.

## 6. Check it against a real GitLab

Once you have a GitLab: add one project, press Test connection, then check that

1. the merge request count matches the project page,
2. a started pipeline shows as a card with the right stage, and its colour and time are sensible,
3. a failing job shows its name in the red alert,
4. the pipeline name column makes sense for your project (see section 4).

If something is off, `deskwatch-bridge` with `RUST_LOG=deskwatch_bridge=debug` logs each poll. Send a sanitised
copy of the answers (with project names, ids and commits made up) and the shapes in
`bridge/src/gitlab/testdata/` can be fixed.
