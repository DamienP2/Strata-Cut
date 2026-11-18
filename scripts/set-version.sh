#!/usr/bin/env bash
# Writes the version of a release tag into Cargo.toml, Cargo.lock and web/package.json.
#   scripts/set-version.sh v1.2      → 1.2.0
#   scripts/set-version.sh           → uses $GITHUB_REF_NAME
set -euo pipefail
cd "$(dirname "$0")/.."

tag="${1:-${GITHUB_REF_NAME:-}}"
v="${tag#v}"
v="${v#V}"
if ! [[ "$v" =~ ^[0-9]+(\.[0-9]+){0,2}$ ]]; then
  echo "not a version tag: '$tag' (expected vX.Y.Z)" >&2
  exit 1
fi
IFS=. read -r major minor patch <<<"$v"
v="${major}.${minor:-0}.${patch:-0}"

# Cargo.toml: the version under [workspace.package]
awk -v v="$v" '
  /^\[/ { section = $0 }
  section == "[workspace.package]" && /^version *=/ && !done { print "version = \"" v "\""; done = 1; next }
  { print }
' Cargo.toml >Cargo.toml.tmp && mv Cargo.toml.tmp Cargo.toml

# Cargo.lock: the version right after our crates' names
awk -v v="$v" '
  /^name = "strata-cut-/ { ours = 1; print; next }
  ours && /^version = / { print "version = \"" v "\""; ours = 0; next }
  { ours = 0; print }
' Cargo.lock >Cargo.lock.tmp && mv Cargo.lock.tmp Cargo.lock

# web/package.json (and the lock's own entries, so `npm ci` agrees)
node -e '
  const fs = require("fs");
  const v = process.argv[1];
  const edit = (f, fn) => { if (!fs.existsSync(f)) return; const j = JSON.parse(fs.readFileSync(f, "utf8")); fn(j); fs.writeFileSync(f, JSON.stringify(j, null, 2) + "\n"); };
  edit("web/package.json", (j) => { j.version = v; });
  edit("web/package-lock.json", (j) => { j.version = v; if (j.packages && j.packages[""]) j.packages[""].version = v; });
' "$v"

echo "version $v"
