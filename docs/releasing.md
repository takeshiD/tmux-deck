# Releasing tmux-deck

Stable releases are fully automated from a single `vX.Y.Z` tag. Do not create
alpha, beta, or release-candidate tags: the release workflow rejects them.

## One-time publisher setup

The repository already uses the `takeshid` Cachix cache and expects the
`CACHIX_AUTH_TOKEN` repository secret to contain a per-cache write token.

Create the crates.io Trusted Publisher once by running:

```console
./scripts/setup-release-publishing.sh
```

The wizard opens the crate settings page and walks through the exact values.
It does not request or store a crates.io API token. The GitHub `release`
Environment must exist without required reviewers because pushing the tag is
the approval operation.

## Preflight the runner matrix

Before the first release, open the **Release** workflow in GitHub Actions and
choose **Run workflow**. A manual run executes preflight and all four native
Rust/Nix builds, but every publication job is skipped.

## Prepare and publish a release

1. Update `package.version` in `Cargo.toml` to the next stable version.
2. Run `cargo check` to update the root package entry in `Cargo.lock`.
3. Open and merge a release PR into the default branch.
4. From the merged commit, create and push the matching annotated tag:

   ```console
   git switch main
   git pull --ff-only
   git tag -a vX.Y.Z -m "Release vX.Y.Z"
   git push origin vX.Y.Z
   ```

The workflow validates the tag, manifest, lockfile, and default-branch
ancestry. It then completes all tests and native builds before publishing to
crates.io and Cachix. GitHub Release is created only after both destinations
finish.

## Recover from a failed release

Do not delete or recreate the tag. In the failed GitHub Actions run, choose
**Re-run failed jobs**. The workflow verifies an existing crates.io package by
checksum, repeats Cachix pushes safely, and replaces existing GitHub assets.

If the tag/version/ancestry gate fails, prepare a new version and tag instead of
moving a published tag.
