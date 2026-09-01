#!/usr/bin/env bash
set -euo pipefail

crate_name="tmux-deck"
default_branch="${DEFAULT_BRANCH:?DEFAULT_BRANCH must be set}"

cargo_version="$(
  cargo metadata --no-deps --format-version 1 --locked |
    jq -er --arg name "${crate_name}" '.packages[] | select(.name == $name) | .version'
)"
lock_version="$(
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
)"

[[ -n "${lock_version}" ]] || {
  echo "could not find ${crate_name} in Cargo.lock" >&2
  exit 1
}
[[ "${cargo_version}" == "${lock_version}" ]] || {
  echo "Cargo.toml ${cargo_version} does not match Cargo.lock ${lock_version}" >&2
  exit 1
}

if [[ "${GITHUB_EVENT_NAME}" == "workflow_dispatch" ]]; then
  {
    echo "publish=false"
    echo "version=${cargo_version}"
  } >> "${GITHUB_OUTPUT}"
  echo "manual run: preflight only for ${crate_name} ${cargo_version}"
  exit 0
fi

[[ "${GITHUB_EVENT_NAME}" == "push" && "${GITHUB_REF_TYPE}" == "tag" ]] || {
  echo "publishing requires a tag push" >&2
  exit 1
}
[[ "${GITHUB_REF_NAME}" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo "tag ${GITHUB_REF_NAME} is not a stable vX.Y.Z release" >&2
  exit 1
}

tag_version="${GITHUB_REF_NAME#v}"
[[ "${tag_version}" == "${cargo_version}" ]] || {
  echo "tag ${tag_version} does not match Cargo.toml ${cargo_version}" >&2
  exit 1
}

git fetch --no-tags origin \
  "+refs/heads/${default_branch}:refs/remotes/origin/${default_branch}"
tag_commit="$(git rev-list -n 1 "${GITHUB_REF_NAME}")"
git merge-base --is-ancestor "${tag_commit}" "origin/${default_branch}" || {
  echo "tag commit ${tag_commit} is not in origin/${default_branch}" >&2
  exit 1
}

{
  echo "publish=true"
  echo "version=${cargo_version}"
} >> "${GITHUB_OUTPUT}"
echo "validated ${GITHUB_REF_NAME} at ${tag_commit} for publication"
