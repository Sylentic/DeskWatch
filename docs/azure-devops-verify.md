# Checking Azure DevOps against a real organisation

The Azure DevOps source works on a real organisation (runs, stages, approvals, pull requests). It was first written
against hand-written mock answers, so a few names and shapes, mostly agent pools and check types, are still
assumptions. This page is the checklist for trying them on your own organisation: what
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

Add the block from [azure-devops.md](azure-devops.md), start the bridge with `RUST_LOG=deskwatch_bridge::azure_devops=debug` (on Linux add
`Environment=RUST_LOG=...` with `systemctl edit deskwatch-bridge`), and open the kiosk. Compare what it shows with what Azure DevOps shows:

| You do | You should see |
|---|---|
| Run a pipeline | It appears in the Running now widget with stage and task, and moves with the stages |
| Fail a task on purpose | Red alert naming the failed task |
| Run a pipeline with an approval | One "Approval needed" notice and a `review` row; the job screen is withdrawn |
| Same with a business-hours or branch check | After about 10 seconds a "Check pending" notice and a `review` row |
| Stop the only agent in a pool | The agent shows offline, with `n offline` in the Runners card |
| Queue a job with no free agent | A row `<pool>: n waiting` |
| Click a PR, a run, a running job and a pool on the kiosk | Each opens the same item in Azure DevOps (a pull request page, the build results page, the pool in the organisation settings). If one lands on the wrong page, note which and what the address looked like with the organisation and project replaced |
| Remove the Agent Pools scope from the token | `cannot poll agent pools` in the log, the pipelines and PR cards stay fresh |

The log lines to grep for: `polling Azure DevOps`, `polled agents`, `polled job requests`, `waiting`,
`cannot poll agent pools`, `no self-hosted agent pool with this name`.

## 4. Record the answers safely (`ado-capture`)

Instead of copying JSON by hand, the bridge can record every answer from section 2 for you and hide all names:

```sh
# Linux or macOS: type the token without it ending up in your shell history
read -rs AZURE_DEVOPS_PAT; export AZURE_DEVOPS_PAT

cargo run -p deskwatch-bridge -- ado-capture --organization your-org --project "Your Project" \
  --hide "yourcompany" --hide "someone@example.com"
```

On Windows PowerShell set the token with `$env:AZURE_DEVOPS_PAT = Read-Host "PAT"` first (or use `--token-file`). The
`ado-capture` command is part of the bridge, so run it from a clone of the repository as above
(`cargo run`) until then. Useful options: `--project` more than once, `--pool "Pool name"` for the self-hosted pool
you care about, `--build 1234` for the run that is waiting on an approval or check right now (start one first, then
pass its id), `--timelines 15` for more history. `--help` lists them all.

What it does:

- It sends only the same `GET` requests as the bridge: builds, the timelines of the newest builds, open pull
  requests, agent pools, and the agents and job requests of the self-hosted pools.
- It writes one file, `ado-capture.json`. The token is never printed or saved.
- Every name is replaced by a placeholder that stays the same inside the file (`definition-1`, `agent-2`,
  `user-1@example.invalid`), so the relations stay: which build belongs to which pipeline, which agent runs which job.
  Ids are renumbered, links are replaced, branches other than `main` become `branch-1`, commit ids and GUIDs are faked.
  Kept as they are: statuses and results, record types such as `Checkpoint.Approval`, timestamps, counts, order and
  booleans. Build parameters, variables and links are dropped.
- Anything the tool does not know is hidden, not kept. The organisation, the project names, every `--hide` word and
  the token are searched for once more in the result; one hit and the file is not written.
- At the end it prints which requests worked (`ok`) and which did not (`FAILED ... HTTP 403`), for example a missing
  scope.

Open the file and read it before you share it; names are replaced, but you are the judge. The "report" at the top of
the file lists the combinations of status and result, and the timeline record types and states, that it saw.

### What to share

1. `ado-capture.json`, once with something running or waiting if you can (a queued run, a run on an approval, a
   run on a business-hours check), and once at rest.
2. In a sentence: what the Azure DevOps website showed at that moment (for example "one run waiting on approval,
   agent Pool X had one busy and one idle agent"), so the data can be compared with what you saw.

## 5. Report back

For anything that differs, an issue with the sanitised JSON of the one record (not the whole answer) is enough: the
fixtures in `bridge/src/azure_devops/testdata/` are written to match, and the parser is only a few lines per shape.
Captured files from section 4 can be turned into new fixtures and compared with the hand-written ones.
