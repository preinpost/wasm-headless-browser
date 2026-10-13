#!/usr/bin/env bash
# Bumps the workspace semver used by the release.
#
# The version lives in Cargo.toml under [workspace.package]. Workspace members
# inherit it. Cargo.lock records those package versions and is updated to match.
# Prints the new version on stdout and nothing else.
#
#   scripts/bump-version.sh patch|minor|major
#
# The release workflow calls this, builds the engine, then tags v<version>.
# A failed build does not push, so a failed run does not consume the version.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

level="${1:-}"
case "$level" in
  patch|minor|major) ;;
  *)
    echo "usage: scripts/bump-version.sh patch|minor|major" >&2
    exit 2
    ;;
esac

current="$(
  awk '
    $0 == "[workspace.package]" { in_block = 1; next }
    in_block && /^version = "/ {
      gsub(/^version = "|"$/, "", $0)
      print
      exit
    }
    in_block && /^\[/ { exit }
  ' Cargo.toml
)"

if [[ ! "$current" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]]; then
  echo "workspace version is not major.minor.patch: ${current:-<missing>}" >&2
  exit 1
fi

major="${BASH_REMATCH[1]}"
minor="${BASH_REMATCH[2]}"
patch="${BASH_REMATCH[3]}"

case "$level" in
  major)
    major=$((major + 1))
    minor=0
    patch=0
    ;;
  minor)
    minor=$((minor + 1))
    patch=0
    ;;
  patch)
    patch=$((patch + 1))
    ;;
esac

new="${major}.${minor}.${patch}"

awk -v new="$new" '
  $0 == "[workspace.package]" { in_block = 1; print; next }
  in_block && !done && /^version = "/ {
    print "version = \"" new "\""
    done = 1
    in_block = 0
    next
  }
  in_block && /^\[/ { in_block = 0 }
  { print }
  END { if (!done) exit 1 }
' Cargo.toml > Cargo.toml.tmp
mv Cargo.toml.tmp Cargo.toml

# Only the workspace packages. A dependency that happens to share the old
# version string must be left alone.
awk -v new="$new" '
  BEGIN {
    members["browser"] = 1
    members["browser-core"] = 1
    members["cdp-wire"] = 1
    members["engine"] = 1
    members["engine-fake"] = 1
    members["whb-client"] = 1
  }
  /^name = "/ {
    name = $3
    gsub(/"/, "", name)
    hit = (name in members)
    print
    next
  }
  hit && /^version = "/ {
    print "version = \"" new "\""
    updated++
    hit = 0
    next
  }
  { hit = 0; print }
  END { if (updated != 6) exit 1 }
' Cargo.lock > Cargo.lock.tmp
mv Cargo.lock.tmp Cargo.lock

printf '%s\n' "$new"
