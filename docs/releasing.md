# Releasing

A release is a version tag on `main`. Everything runs in one workflow,
[`.github/workflows/pipeline.yml`](../.github/workflows/pipeline.yml): for a tag like `v1.0.1` it runs the checks, checks
the tag against the crate version, builds the Linux x86_64, Linux aarch64 and Windows archives and the multi-arch image
`ghcr.io/sylentic/deskwatch-bridge`, and publishes the GitHub release.

## Branches

- **`dev`** is the default branch. All work goes into `dev` through a pull request with a green pipeline.
- **`main`** is what was released. It only takes a pull request **from `dev`**: the `from dev` job fails for any
  other branch, and the repository's rulesets require that job and the checks. Nobody pushes to `main` or `dev`
  directly, and neither can be deleted or force-pushed.

## Steps

1. On a branch from `dev`, prepare the release and merge it into `dev` (a normal pull request). It changes these
   places:
   - `version` in the root `Cargo.toml`, and `Cargo.lock` with it (the pipeline builds with `--locked`);
   - `version` in `firmware/Cargo.toml` and `firmware/Cargo.lock`;
   - `Unreleased` in `CHANGELOG.md`, turned into the new version section (the release notes are that section);
   - the image tag in `deploy/docker-compose.yml`;
   - the version examples in `docs/install.md` (two), `docs/raspberry-pi.md` and `docs/windows.md` (two), and the
     example in `docs/docker.md` section 10.
2. Open the release pull request **from `dev` into `main`**, titled "Release x.y.z". Merge it with a merge commit
   once the pipeline is green (it also builds the image for both CPUs).
3. Update your checkout so the tag lands on that merge commit:

   ```sh
   git checkout main
   git pull
   git log -1 --oneline    # must be "Merge pull request ... from Sylentic/dev"
   ```

4. Tag that commit and push the tag:

   ```sh
   git tag v1.0.1
   git push origin v1.0.1
   ```

The `release version` job refuses to go on when the tag does not match the crate version at the tagged commit. That is
what happens when the tag is made on a commit from before the release pull request was merged.

## A tag on the wrong commit

Re-running the failed run does not help, because a re-run uses the same commit. Move the tag instead (this deletes and
recreates it, so do it only when the release never finished and nobody has pulled the tag):

```sh
git tag -d v1.0.1
git push origin :refs/tags/v1.0.1
git tag v1.0.1 <merge commit of the release PR>
git push origin v1.0.1
```

## The image tag in the Compose file

`deploy/docker-compose.yml` pins the image to a release. The image only exists after the pipeline has finished for the
tag, so check that `ghcr.io/sylentic/deskwatch-bridge:<version>` can be pulled before announcing the release. If the run
failed, the pinned tag does not exist and `docker compose pull` fails with "manifest unknown" until the release is fixed.

## v0.9.8 was never released

The `v0.9.8` tag was pushed on the commit before the release PR was merged, so every job stopped at the version check.
No binaries, image or GitHub release exist for 0.9.8, and the tag was left as it is. Its changes shipped in 0.9.9.
