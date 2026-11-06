#!/usr/bin/env bash
# Publish a new version:  scripts/release.sh 0.2.0
# Bumps the version in Cargo.toml and tauri.conf.json, commits, tags v0.2.0
# and pushes. GitHub Actions then builds, signs and publishes the release,
# and installed apps offer the update.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"

VERSION="${1:-}"
if ! [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "usage: $0 X.Y.Z" >&2
  exit 1
fi
if [[ -n "$(git status --porcelain)" ]]; then
  echo "Working tree is not clean; commit or stash first." >&2
  exit 1
fi
if git rev-parse "v$VERSION" >/dev/null 2>&1; then
  echo "Tag v$VERSION already exists." >&2
  exit 1
fi

python3 - "$VERSION" <<'PY'
import json, re, sys
v = sys.argv[1]
p = "Cargo.toml"
s = open(p).read()
s = re.sub(r'(\[workspace\.package\][^\[]*?version = ")[^"]+(")', rf'\g<1>{v}\2', s, count=1, flags=re.S)
open(p, "w").write(s)
p = "app/tauri.conf.json"
c = json.load(open(p))
c["version"] = v
with open(p, "w") as f:
    json.dump(c, f, indent=2)
    f.write("\n")
PY
cargo update --workspace --quiet

git add Cargo.toml Cargo.lock app/tauri.conf.json
git commit -m "Release v$VERSION"
git tag -a "v$VERSION" -m "v$VERSION"
git push origin HEAD "v$VERSION"
echo "Pushed v$VERSION. Follow the build: gh run watch \$(gh run list -w release -L1 --json databaseId -q '.[0].databaseId')"
