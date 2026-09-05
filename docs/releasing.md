# Releasing tmux-deck

Stable releases are fully automated from a single `vX.Y.Z` tag. Do not create
alpha, beta, or release-candidate tags: the release workflow rejects them.

## Choose publication targets

GitHub release artifacts are always published after every required build
succeeds. Registry publication is controlled independently with repository
variables:

| Variable | `true` means |
| --- | --- |
| `PUBLISH_CACHIX` | Build and push all supported Nix systems to `takeshid` |
| `PUBLISH_CRATES_IO` | Publish the crate through OIDC trusted publishing |

An unset variable behaves as `false`. Configure the intended targets and any
required credentials by running:

```console
./scripts/setup-release-publishing.sh
```

The wizard records both repository variables. When Cachix is enabled, confirm
that the existing `CACHIX_AUTH_TOKEN` is a `takeshid`-scoped write token. When
crates.io is enabled, register its GitHub Trusted Publisher; no long-lived API
token is stored. The GitHub `release` Environment is needed only for crates.io.

## Preflight the runner matrix

Before the first release, open the **Release** workflow in GitHub Actions and
choose **Run workflow**. A manual run executes preflight and all four native
Rust/Nix builds, but every publication job is skipped.

## Prepare and publish a release

1. Update `package.version` in `Cargo.toml` to the next stable version.
2. Run `cargo check` to update the root package entry in `Cargo.lock`.
3. Open and merge a `release/*` PR into the default branch. Other pull requests
   are rejected if they change the package version.
4. From the merged commit, create and push the matching annotated tag:

   ```console
   git switch main
   git pull --ff-only
   git tag -a vX.Y.Z -m "Release vX.Y.Z"
   git push origin vX.Y.Z
   ```

The workflow validates the tag, manifest, lockfile, and default-branch
ancestry. It then completes all tests and native builds before publishing to
each enabled registry. GitHub Release is created only after the required builds
and all enabled destinations finish. A disabled registry does not block it.

## Recover from a failed release

Do not delete or recreate the tag. In the failed GitHub Actions run, choose
**Re-run failed jobs**. When enabled, the workflow verifies an existing
crates.io package by checksum, repeats Cachix pushes safely, and replaces
existing GitHub assets.

If the tag/version/ancestry gate fails, prepare a new version and tag instead of
moving a published tag.
