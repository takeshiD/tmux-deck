#!/usr/bin/env bash
set -euo pipefail

crate_name="tmux-deck"
base_sha="${BASE_SHA:?BASE_SHA must be set}"
head_ref="${HEAD_REF:?HEAD_REF must be set}"

manifest_version="$({
  cargo metadata --no-deps --format-version 1 --locked |
    jq -er --arg name "${crate_name}" '.packages[] | select(.name == $name) | .version'
})"
lock_version="$({
  awk -v crate="${crate_name}" '
    /^\[\[package\]\]$/ { in_package = 1; name = ""; next }
    in_package && /^name = / {
      name = $0
      sub(/^name = "/, "", name)
      sub(/"$/, "", name)
      next
    }
    in_package && name == crate && /^version = / {
      version = $0
      sub(/^version = "/, "", version)
      sub(/"$/, "", version)
      print version
      exit
    }
  ' Cargo.lock
})"

[[ -n "${lock_version}" && "${manifest_version}" == "${lock_version}" ]] || {
  echo "Cargo.toml ${manifest_version} does not match Cargo.lock ${lock_version:-missing}" >&2
  exit 1
}

base_version="$(
  git show "${base_sha}:Cargo.toml" |
    sed -n -E 's/^version[[:space:]]*=[[:space:]]*"([^"]+)"/\1/p' |
    head -n1
)"
[[ -n "${base_version}" ]] || {
  echo "could not read the base package version from ${base_sha}" >&2
  exit 1
}

if [[ "${head_ref}" == release/* ]]; then
  echo "release branch ${head_ref}: allowing ${base_version} -> ${manifest_version}"
elif [[ "${manifest_version}" != "${base_version}" ]]; then
  echo "only release/* branches may change the package version" >&2
  echo "${head_ref} changes ${base_version} -> ${manifest_version}" >&2
  exit 1
else
  echo "non-release branch keeps package version ${manifest_version}"
fi
