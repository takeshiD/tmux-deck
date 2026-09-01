# ADR 0001: Tag-triggered releases

- Status: Accepted
- Date: 2026-09-02

## Context

tmux-deck was released through separate manual and automated paths. GitHub
Release and Cachix publication were tag-triggered, while crates.io publication
was manual. The pull-request workflow also required a placeholder version,
which conflicted with using `Cargo.toml` as the version shown by builds.

## Decision

A stable `vX.Y.Z` tag is the single release approval and trigger.
`Cargo.toml` is the version source of truth, and `Cargo.lock` must contain the
same package version. The tagged commit must be in the default branch history.

All preflight checks and all four native Rust and Nix builds must finish before
publication starts. crates.io uses GitHub OIDC trusted publishing; Cachix uses
its existing per-cache write token. GitHub Release is created last and means
that every publication path completed.

Manual workflow runs are preflight-only. Re-running the same tag converges
rather than requiring tag deletion: an existing crates.io package must have the
same checksum, Cachix accepts repeated pushes, and GitHub assets are replaced.

## Consequences

- Stable releases cannot be published from prerelease tags or side branches.
- Release preparation changes both `Cargo.toml` and `Cargo.lock` before tagging.
- crates.io needs a one-time Trusted Publisher registration for the `release`
  GitHub Environment.
- Four native runner families are release gates, increasing release runtime in
  exchange for testing the exact published architectures.
