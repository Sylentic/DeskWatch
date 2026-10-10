# Runners and agent pools

Which of your CI runners are up, busy or down, on the kiosk dashboard. This page is the design for all three CI
systems and the guide for the parts that exist today: **Gitea runners** and **GitHub self-hosted runners**.

| Source | What it lists | State |
|---|---|---|
| Gitea | Runners of a user, organisation, repository or the whole instance | In this version |
| GitHub, GitHub Enterprise | Self-hosted runners of a repository or organisation | In this version |
| Azure DevOps | Agents of the self-hosted agent pools, plus queued jobs per pool | After that, mocks only until the token question is settled |
| ESP panel | A `runners` page and an offline count in the `warn` badge | Last PR, needs a schema change |

## 1. How it fits

Every source turns what it reads into the same fact, so the page does not care where a runner comes from:

```text
Runner { source, name, status: offline | busy | idle, disabled, labels }
```

- A source sends `Update::Runners { key, runners }` for one **scope** (a user, an organisation, a repository, a
  pool). The key is `<source>:<name>:runners:<scope>`, for example `gitea:home:runners:org:my-team`. The update
  **replaces** the scope's whole list, so a runner that was deleted disappears on the next poll, and an empty list
  clears the scope.
- The facts remember when each scope was refreshed. A list that nobody refreshed for an hour (the source stopped
  asking, or the token lost its rights) is dropped. Before that, the kiosk greys the card out after 150 seconds
  (a few missed polls), the same way it greys the pipelines widget when a source is down.
- `disabled` means an administrator switched the runner off on purpose. It is shown grey, sorted last, and never
  counts as an outage.
- A status the bridge does not recognise counts as **offline**. Showing a runner as idle when we cannot tell would
  hide an outage.
- Scopes that overlap (the `admin` list plus an organisation) list the same runner twice. Pick one.

The kiosk snapshot has a new top-level `runners` list (version stays 1, a page that does not know it ignores it):

```json
{ "source": "gitea", "name": "runner-1", "status": "busy", "disabled": false,
  "labels": ["ubuntu-latest"], "updated": 1760088000 }
```

### What is not in the first version

- **The ESP panel.** A `runners` page and an offline count in the `warn` badge change the MQTT schema, which has a
  contract with the firmware. That waits until the three sources are in, so the schema is changed once.
- **"Waiting for runner" on the pipelines page** (a queued run with no online runner that has its label). Best
  effort and label matching differs per system, so it follows the schema change.
- **Queued jobs per pool.** Only Azure DevOps shows them cheaply (section 4); the fact grows an optional count in
  that PR.

## 2. Gitea runners (this version)

Gitea 28.1.0 lists runners on four endpoints, one per scope. Each answers `{ "runners": [...], "total_count": n }`
and each runner has `name`, `status`, `busy`, `disabled`, `ephemeral` and `labels`. The names and shapes were read
from the 28.1.0 swagger; the bridge reads the `status` strings `idle`, `active` and `offline` (older builds said
`online`) and also looks at the `busy` flag.

| `runners` entry | Endpoint | Token |
|---|---|---|
| `"user"` | `GET /api/v1/user/actions/runners` | The runners of the token's user |
| `"org:<name>"` | `GET /api/v1/orgs/<name>/actions/runners` | Needs access to that organisation |
| `"repo:<owner>/<name>"` | `GET /api/v1/repos/<owner>/<name>/actions/runners` | Needs access to that repository |
| `"admin"` | `GET /api/v1/admin/actions/runners` | Administrator token, lists every runner |

**Assumption, to check on first use:** the scopes Gitea asks for on each endpoint (`read:user`, `read:organization`,
`read:repository`, and an administrator token for `admin`). A token without the right to read a scope gets
`403`; the bridge logs `cannot poll runners` with the scope and keeps showing the last list (greyed out later).
Prefer `user` or `org:...` with a read-only token over `admin`.

Config, in the Gitea block (`token_file` is needed, because runner lists are not public):

```toml
[[source.gitea]]
name = "home"
base_url = "https://gitea.example.com"
webhook_secret_file = "gitea-webhook-secret"
token_file = "gitea-token"
runners = ["org:my-team"]     # off when empty or missing
runner_poll_s = 30            # seconds between polls
```

Then add the widget to the kiosk layout. It is not in the default layout, because that grid is already full and
runners need a token:

```toml
[[kiosk.panel]]
widget = "runners"
span = [1, 1]
# rows = 6                    # cap the list, the card says "+n more"
```

Gitea pages its answers (50 per page by default). The bridge reads every page, at most 20.

## 3. GitHub self-hosted runners (in this version)

Same `Runner` fact, same `runners = [...]` grammar as Gitea, but only `"repo:<owner>/<name>"` and `"org:<name>"`
(GitHub has no user or admin list here). `runner_poll_s` defaults to 300.

```toml
[[source.github]]
name = "personal"
token_file = "github-personal"
repos = ["your-user/repo-a"]
runners = ["repo:your-user/repo-a"]   # or "org:your-org"; off when empty or missing
runner_poll_s = 300
```

The runner poll is separate from the PR and run poll and **does not change the source's health**. A token
without the runner right gets a plain `403` on this list only; the bridge logs `cannot poll runners` with the
scope and the PR and pipeline widgets stay fresh. Add the widget as in section 2.

| Scope | Endpoint | Token scope |
|---|---|---|
| Repository | `GET /repos/{owner}/{repo}/actions/runners` | Fine-grained: repository permission **Administration: read**. Classic token: `repo` |
| Organisation | `GET /orgs/{org}/actions/runners` | Fine-grained: organisation permission **Self-hosted runners: read**. Classic token: `admin:org` |

This is the scope that surprises people: GitHub files the runner list under *Administration*, so a token that was
made for pull requests and Actions only gets `403` here. Adding **Administration: read** to a fine-grained token
is a wide read right (it also exposes repository settings), so prefer an organisation token with only
**Self-hosted runners: read** when the runners belong to an organisation. The bridge only ever sends `GET`
requests. Skipping `runners` needs no extra rights.

Each runner has `name`, `status` (`online` or `offline`), `busy` and `labels` (objects with a `name`). GitHub-hosted
runners are not listed, which is fine: only self-hosted ones can be down. Polling every 5 minutes is enough and
costs 1 request per scope; conditional requests with an ETag do not count against the rate limit on github.com.
GitHub Enterprise Server and GHE.com use the same paths below their `base_url`. Mapping: `offline` is offline;
`online` with `busy` is busy; `online` otherwise is idle.

## 4. Azure DevOps agent pools (after GitHub)

Azure DevOps calls them agents in pools. Only **self-hosted** pools matter (a Microsoft-hosted pool is `isHosted`
and skipped).

| What | Endpoint (`api-version=7.1`) | Notes |
|---|---|---|
| Pools | `GET https://dev.azure.com/{org}/_apis/distributedtask/pools` | Skip `isHosted: true` |
| Agents of a pool | `GET .../pools/{poolId}/agents?includeAssignedRequest=true` | `status` is `online` or `offline`, `enabled` is false when disabled, `assignedRequest` is set while the agent runs a job |
| Queued jobs | `GET .../pools/{poolId}/jobrequests` | **Assumption:** a request with no `assignTime` and no `finishTime` is waiting for an agent. Verify before relying on it |

- **Token:** the existing read-only PAT needs one more scope, **Agent Pools (Read)**. That is a change to the
  token, and the work organisation's policy on tokens is not settled, so this source is developed against recorded
  mock responses only, the same as the rest of the Azure DevOps source. Skipping `agent_pools` needs no new rights.
- Mapping: `offline` is offline; `online` with an `assignedRequest` is busy; `online` otherwise is idle; `enabled:
  false` is disabled.
- Scope key: one per pool, `azure_devops:<name>:runners:pool:<pool>`, with an optional `queued` count added to the
  fact then.
- Polling every 60 seconds is plenty; it is one request per pool plus the pool list.

## 5. Order of the work

1. **This PR:** the `Runner` fact and the kiosk widget, Gitea as the first source, config, docs, tests.
2. **GitHub runners (done):** its own PR, because it brings a second token scope and its own API tests.
3. **Azure DevOps pools:** its own PR, mocks only, after the token scope is agreed.
4. **ESP panel:** the `runners` page, the offline count in the `warn` badge and "waiting for runner" on the
   pipelines page, together with one amendment of the MQTT schema.

Steps 2 and 3 share nothing with each other except the fact, so they can be built in either order or in parallel.

## 6. Try it without any CI

```sh
deskwatch-bridge --demo --kiosk --no-mqtt config.toml    # config with a `runners` panel
```

The demo has four fake runners: one that flips between busy and idle, one idle, one offline and one disabled.
