# Releasing

A release happens by itself when `dev` is merged into `main` with a new version number. Everything runs in one
workflow, [`.github/workflows/pipeline.yml`](../.github/workflows/pipeline.yml): on the push to `main` it runs the
checks, and when the version in `Cargo.toml` has no tag yet, it tags the merged commit (`v1.0.1`), builds the Linux
x86_64, Linux aarch64 and Windows archives and the multi-arch image `ghcr.io/sylentic/deskwatch-bridge`, and publishes
the GitHub release. A merge into `main` without a new version releases nothing.

## Branches

- **`main`** is the default branch, the page people land on, and what was released. It only takes a pull request
  **from `dev`**: the `from dev` job fails for any other branch, and the repository's rulesets require that job and
  the checks.
- **`dev`** is where work goes first: every new branch starts from `dev` and comes back through a pull request with a
  green pipeline. Once it is tested there, it goes to `main` with the next pull request from `dev`.
- Nobody pushes to `main` or `dev` directly, and neither can be deleted or force-pushed.

## Steps

1. On a branch from `dev`, prepare the release and merge it into `dev` (a normal pull request). It changes these
   places:
   - `version` in the root `Cargo.toml`, and `Cargo.lock` with it (the pipeline builds with `--locked`);
   - `Unreleased` in `CHANGELOG.md`, turned into the new version section (the release notes are that section; the pipeline adds the closing line "Something not working? Open an issue" by itself);
   - the image tag in `deploy/docker-compose.yml`;
   - the version examples in `docs/install.md` (two), `docs/raspberry-pi.md` and `docs/windows.md` (two), and the
     example in `docs/docker.md` section 10.
2. Open the release pull request **from `dev` into `main`**, titled "Release x.y.z". Merge it with a merge commit
   once the pipeline is green (it also builds the image for both CPUs).
3. That is all. The run on `main` (Actions › Pipeline) tags the merge commit in its `release version` job and
   publishes the release a few minutes later. Check the release page and that the image pulls (below).

A version tag pushed by hand still works too (for example to retry a release that never finished): the run checks
that the tag matches the crate version at that commit, and refuses when it does not.

## A tag on the wrong commit

When a release run on `main` fails after it made the tag (a package or the image), use **Re-run failed jobs** on that
run: the jobs that passed, `release version` among them, keep their result, so the release continues with the same tag.
(**Re-run all jobs** does not release: the tag exists by then.)

A tag pushed by hand on the wrong commit is refused by `release version`; re-running does not help, because a re-run
uses the same commit. Move the tag instead (this deletes and recreates it, so do it only when the release never
finished and nobody has pulled the tag):

```sh
git tag -d v1.0.1
git push origin :refs/tags/v1.0.1
git tag v1.0.1 <merge commit of the release PR>
git push origin v1.0.1
```

## The image tag in the Compose file

`deploy/docker-compose.yml` uses the `latest` tag, so a release needs no change there. The pipeline moves `latest` to a
new release (not to a pre-release such as `-rc.1`) once it has finished for the tag, so check that
`ghcr.io/sylentic/deskwatch-bridge:<version>` can be pulled before announcing the release.

## v0.9.8 was never released

The `v0.9.8` tag was pushed on the commit before the release PR was merged, so every job stopped at the version check.
No binaries, image or GitHub release exist for 0.9.8, and the tag was left as it is. Its changes shipped in 0.9.9.
