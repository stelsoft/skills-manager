#!/usr/bin/env bash
# Sync fork branch with upstream, rebuild deb in docker. Usage: ./build-deb.sh [--install] [--no-sync]
set -euo pipefail
cd "$(dirname "$0")"
BRANCH=extra-files-backup
IMG=skills-manager-build
TARGET=${TARGET:-$HOME/.cache/skills-manager-target}
INSTALL=0 SYNC=1
for a in "$@"; do case $a in --install) INSTALL=1;; --no-sync) SYNC=0;; *) echo "unknown arg $a"; exit 2;; esac; done

if [ $SYNC = 1 ]; then
  git diff --quiet && git diff --cached --quiet || { echo "working tree dirty, commit or stash first"; exit 1; }
  git checkout -q "$BRANCH"
  git fetch -q upstream --tags
  TAG=$(git tag --sort=-creatordate | head -1)
  echo "merging $TAG"
  git merge --no-edit "$TAG"
  git push -q origin "$BRANCH"
fi

[ -d node_modules ] && [ package-lock.json -ot node_modules ] || npm ci
npm run build

docker image inspect "$IMG" >/dev/null 2>&1 || docker build -t "$IMG" - <<'DF'
FROM rust:1-bookworm
RUN apt-get update -qq && apt-get install -y -qq libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev libssl-dev pkg-config git nodejs npm >/dev/null && rm -rf /var/lib/apt/lists/*
DF

mkdir -p "$TARGET"
docker run --rm -v "$PWD":/w -v "$TARGET":/w/src-tauri/target -w /w "$IMG" \
  npx tauri build --bundles deb --config '{"build":{"beforeBuildCommand":""},"bundle":{"createUpdaterArtifacts":false}}'

DEB=$(ls -t "$TARGET"/release/bundle/deb/*.deb | head -1)
echo "DEB: $DEB"
[ $INSTALL = 1 ] && sudo dpkg -i "$DEB"
exit 0
