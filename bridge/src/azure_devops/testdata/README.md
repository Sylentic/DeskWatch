# Test fixtures

Sanitised copies of the shapes Azure DevOps returns (API version 7.1), trimmed to the fields the bridge reads and
written by hand. The organisation is `your-org`, the project `project-a`, repositories `repo-a` and `repo-b`; ids
are made up. Nothing here comes from a real organisation.

- `builds.json`: `GET {project}/_apis/build/builds`. Five builds of four pipelines: one queued, one deploy running,
  one failed build, an older deploy (hidden behind the newer one) and a cancelled build.
- `timeline_running.json`: `.../builds/3004/timeline`. A deploy with a finished `plan` stage and an `apply` stage
  at its fourth task.
- `timeline_approval.json`: the same deploy with the `apply` stage waiting on an environment approval
  (`Checkpoint.Approval`, in progress).
- `timeline_failed.json`: a build whose `Run unit tests` task failed (single implicit stage `__default`).
- `pulls.json`: `GET {project}/_apis/git/pullrequests`. Three active PRs in two repositories, one draft.
