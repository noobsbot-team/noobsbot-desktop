#!/usr/bin/env bash
# Sets the desktop's version (1.0.1, or a beta like 1.0.1-beta.2) in the root Cargo.toml and its Cargo.lock entry.
# Only CI runs it (version.yml, release.yml); the settings crate keeps 0.1.0, as Settings shows the root version.
set -euo pipefail
v=${1#v}
[[ $v =~ ^[0-9]+\.[0-9]+\.[0-9]+(-beta\.[0-9]+)?$ ]] || { echo "not a version: $1" >&2; exit 1; }
cd "$(dirname "$0")/.."
sed -i "0,/^version = .*/s//version = \"$v\"/" Cargo.toml
sed -i "/^name = \"noobsbot-desktop\"$/{n;s/^version = .*/version = \"$v\"/}" Cargo.lock
