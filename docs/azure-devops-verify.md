# Checking Azure DevOps against a real organisation

The Azure DevOps source was written and tested against hand-written mock answers. Nobody has run it against a real
organisation yet, so a few names and shapes are assumptions. This page is the checklist for the first real run: what
to look at, what the bridge expects, and what to do when it differs. Setting up the token and the config is in
[azure-devops.md](azure-devops.md); this page assumes that is done.

**Token policy first.** A personal access token acts as you. If the organisation is your employer's, check that PATs
are allowed and that a token on a home server is acceptable before creating one. For a first test, a personal
organisation (free at dev.azure.com) with one small pipeline avoids the question entirely.

Nothing here goes into the repository: no token, no organisation name, no project names. When you paste output into
an issue, replace names with placeholders first.

## 1. Minimal token

Scopes (Custom defined, Show all scopes), all read-only:

| Scope | Access | Needed for |
|---|---|---|
| Build | Read | Runs, stages, tasks, approvals, checks |
| Agent Pools | Read | Only with `agent_pools` |
| Code | Read | Only with `pull_requests = true` |

## 2. Look at the raw answers

These are the same `GET` requests the bridge sends. Put the token in a variable without it ending up in your shell
history (a space before the command, or read it from the credential file) and replace the placeholders:

```sh
ORG=your-org; PROJECT=project-a
TOKEN=$(cat /etc/deskwatch/credentials/azdo-work)
get() { curl -s -u ":$TOKEN" -H "X-TFS-FedAuthRedirect: Suppress" "$1" ; }

# Builds: is there a recent run, with status and result?
get "https://dev.azure.com/$ORG/$PROJECT/_apis/build/builds?api-version=7.1&\$top=5&queryOrder=queueTimeDescending" | jq '.value[] | {id, status, result, def: .definition.name}'
```

HTML or a sign-in page instead of JSON means the token is refused (expired, wrong organisation, missing scope).

### Agent pools (only with `agent_pools`)

```sh
get "https://dev.azure.com/$ORG/_apis/distributedtask/pools?api-version=7.1" | jq '.value[] | {id, name, isHosted}'
POOL=12   # the id of your self-hosted pool
get "https://dev.azure.com/$ORG/_apis/distributedtask/pools/$POOL/agents?includeAssignedRequest=true&api-version=7.1" | jq '.value[] | {name, status, enabled, busy: (.assignedRequest != null)}'
get "https://dev.azure.com/$ORG/_apis/distributedtask/pools/$POOL/jobrequests?completedRequestCount=0&api-version=7.1" | jq '.value[] | {assignTime, finishTime, result}'
```

| Question | The bridge expects | If it differs |
|---|---|---|
| Pool name | `agent_pools` matches `name` exactly and the pool has `isHosted: false` | Copy the name as printed, including spaces and case |
| Agent `status` | `online` or `offline`, anything else counts as offline | Note the real value |
| `assignedRequest` | Present only while the agent runs a job | If it is always present or never, busy/idle on the kiosk is wrong |
| `enabled` | `false` for an agent an admin disabled | |
| Job requests | `completedRequestCount=0` leaves out finished ones, and a waiting job has neither `assignTime` nor `finishTime` | Queue a job while all agents are busy and look at that request. If finished requests come back, or a waiting one has `assignTime`, the "n waiting" count is wrong |

Test the queue deliberately: disable the only agent (or stop it), start a pipeline that targets the pool, and check
that the job request shows up with no `assignTime`.

### Approvals and other checks

Use a pipeline with an environment that has an approval (and, if you can, a second check such as business hours).
While the run waits, fetch its timeline:

```sh
BUILD=1234   # the id of the waiting run
get "https://dev.azure.com/$ORG/$PROJECT/_apis/build/builds/$BUILD/timeline?api-version=7.1" | jq '.records[] | select(.type | startswith("Checkpoint")) | {type, name, state, result}'
```

| Question | The bridge expects | If it differs |
|---|---|---|
| Approval | A record with `type` exactly `Checkpoint.Approval` and `state` `inProgress` | Note the real `type` |
| Other checks | Records whose `type` starts with `Checkpoint.` (but is not `Checkpoint.Approval`), `inProgress` while the check holds the stage | Note the `type` of each check kind you tried |

## 3. Run the bridge

Add the block from [azure-devops.md](azure-devops.md), start the bridge with debug logging (`RUST_LOG=debug`), and
open the kiosk. Compare what it shows with what Azure DevOps shows:

| You do | You should see |
|---|---|
| Run a pipeline | It appears in the Running now widget with stage and task, and moves with the stages |
| Fail a task on purpose | Red alert naming the failed task |
| Run a pipeline with an approval | One "Approval needed" notice and a `review` row; the job screen is withdrawn |
| Same with a business-hours or branch check | After about 10 seconds a "Check pending" notice and a `review` row |
| Stop the only agent in a pool | The agent shows offline, with `n offline` in the Runners card |
| Queue a job with no free agent | A row `<pool>: n waiting` |
| Remove the Agent Pools scope from the token | `cannot poll agent pools` in the log, the pipelines and PR cards stay fresh |

The log lines to grep for: `polling Azure DevOps`, `polled agents`, `polled job requests`, `waiting`,
`cannot poll agent pools`, `no self-hosted agent pool with this name`.

## 4. Report back

For anything that differs, an issue with the sanitised JSON of the one record (not the whole answer) is enough: the
fixtures in `bridge/src/azure_devops/testdata/` are written to match, and the parser is only a few lines per shape.
