#!/usr/bin/env bash
# Sync fork branch with upstream, rebuild deb in docker. Usage: ./build-deb.sh [--install] [--no-sync]
set -euo pipefail
cd "$(dirname "$0")"
BRANCH=$(git branch --show-current)
IMG=skills-manager-build
TARGET=${TARGET:-$HOME/.cache/skills-manager-target}
INSTALL=0 SYNC=1 PUSH=1
usage() { cat <<U
Usage: $0 [--install] [--no-sync] [--no-push]
  --install   run sudo dpkg -i on the built deb
  --no-sync   skip merging latest upstream tag into current branch
  --no-push   skip pushing fork main + current branch to origin
  -h, --help  this
Env: TARGET=<cargo target dir> (default $TARGET)
U
}
for a in "$@"; do case $a in --install) INSTALL=1;; --no-sync) SYNC=0;; --no-push) PUSH=0;; -h|--help) usage; exit 0;; *) echo "unknown arg $a"; usage; exit 2;; esac; done

if [ $SYNC = 1 ]; then
  git diff --quiet && git diff --cached --quiet || { echo "working tree dirty, commit or stash first"; exit 1; }
  git fetch -q upstream --tags
  git branch -q -f main upstream/main 2>/dev/null || true   # mirror upstream main locally (skipped if checked out)
  [ $PUSH = 1 ] && git push -q origin upstream/main:main
  TAG=$(git tag --sort=-creatordate | head -1)
  echo "merging $TAG"
  git merge --no-edit "$TAG"
  [ $PUSH = 1 ] && git push -q -u origin "$BRANCH"
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

mkdir -p releases
DEB=releases/$(basename "$(ls -t "$TARGET"/release/bundle/deb/*.deb | head -1)")
cp "$TARGET/release/bundle/deb/$(basename "$DEB")" "$DEB"
echo "DEB: $PWD/$DEB"
[ $INSTALL = 1 ] && sudo dpkg -i "$DEB"
exit 0
