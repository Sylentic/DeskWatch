# Releasing

A release is a version tag on `main`. The Release workflow
(`.github/workflows/release.yml`) runs when a tag like `v0.9.9` is pushed and
builds the Linux x86_64, Linux aarch64 and Windows archives, the multi-arch
image `ghcr.io/sylentic/deskwatch-bridge`, and the GitHub release.

## Steps

1. Open the release PR: bump `version` in the root `Cargo.toml`, turn
   `Unreleased` in `CHANGELOG.md` into the new version section, set the image
   tag in `deploy/docker-compose.yml` and update the version examples in the
   install docs. Merge it once CI is green.
2. Update your checkout so the tag lands on the release PR's merge commit:

   ```sh
   git checkout main
   git pull
   git log -1 --oneline    # must be "Merge pull request ... Release x.y.z"
   ```

3. Tag that commit and push the tag:

   ```sh
   git tag v0.9.9
   git push origin v0.9.9
   ```

The workflow refuses to build when the tag does not match the crate version at
the tagged commit. That is what happens when the tag is made on a commit from
before the release PR was merged: every job stops at "Check the tag against the
crate version".

## A tag on the wrong commit

Re-running the failed workflow does not help, because a re-run uses the same
commit. Move the tag instead (this deletes and recreates it, so do it only when
the release never finished and nobody has pulled the tag):

```sh
git tag -d v0.9.9
git push origin :refs/tags/v0.9.9
git tag v0.9.9 <merge commit of the release PR>
git push origin v0.9.9
```

## The image tag in the Compose file

`deploy/docker-compose.yml` pins the image to a release. The image only exists
after the workflow has finished, so check that
`ghcr.io/sylentic/deskwatch-bridge:<version>` can be pulled before announcing
the release. If the workflow failed, the pinned tag does not exist and
`docker compose pull` fails with "manifest unknown" until the release is fixed.

## v0.9.8 was never released

The `v0.9.8` tag was pushed on the commit before the release PR was merged, so every job stopped at the version
check. No binaries, image or GitHub release exist for 0.9.8, and the tag was left as it is. Its changes shipped in
0.9.9. Only tag after the release PR is merged and `git log -1` shows the bumped version.
