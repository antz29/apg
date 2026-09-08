#!/usr/bin/env bash
set -euo pipefail

# apg release helper (steps 4-5 of "Deploying a release" in AGENTS.md).
#
# Repoints the 7 Homebrew formulae (Formula/*.rb) at a new tag and creates the
# annotated release tag — in the CORRECT order, so that when you push the tag
# the bottle workflow builds bottles of the NEW version, not the previous one
# (the bottle job builds from the tap formulae on `main`, so they must already
# point at the new version before the tag is pushed).
#
# Run AFTER committing the release content with the version bumped in
# Cargo.toml + Cargo.lock (that commit is the release HEAD). The script
# verifies the version and a clean tree, rewrites the formulae, commits the
# formula revisions, and creates the annotated tag at the release HEAD. It
# NEVER pushes — pushing and tagging remain human-approved acts; it prints the
# exact commands for you to run.
#
#   scripts/release.sh 0.10.4

NEW_VERSION="${1:?usage: scripts/release.sh <new-version>   e.g. scripts/release.sh 0.10.4}"
TAG="v${NEW_VERSION}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# --- Preconditions ----------------------------------------------------------
if git rev-parse "$TAG" >/dev/null 2>&1; then
  echo "error: tag $TAG already exists (git tag -d $TAG to redo)" >&2
  exit 1
fi
if ! grep -q "^version = \"${NEW_VERSION}\"" Cargo.toml; then
  echo "error: Cargo.toml is not at version = \"$NEW_VERSION\"." >&2
  echo "       Bump Cargo.toml + Cargo.lock and commit the release content first," >&2
  echo "       then re-run this script from the release commit." >&2
  exit 1
fi
if [ -n "$(git status --porcelain)" ]; then
  echo "error: working tree is not clean — commit or stash before releasing." >&2
  git status --short >&2
  exit 1
fi

RELEASE_SHA="$(git rev-parse HEAD)"
echo "==> Release HEAD: $RELEASE_SHA"

# --- Repoint every formula at the new tag -----------------------------------
export APG_TAG="$TAG"
export APG_SHA="$RELEASE_SHA"
for f in Formula/*.rb; do
  perl -0pi -e '
    s/tag:\s+"v[0-9]+\.[0-9]+\.[0-9]+"/tag:      "$ENV{APG_TAG}"/;
    s/revision:\s+"[0-9a-f]{40}"/revision: "$ENV{APG_SHA}"/;
    s#(releases/download/)v[0-9]+\.[0-9]+\.[0-9]+"#$1$ENV{APG_TAG}"#;
    s/(rebuild )(\d+)/$1.($2+1)/ge;
  ' "$f"
done

echo "==> Committing formula revisions"
git add Formula
git commit -m "Point formula revisions at the $TAG release HEAD

Formula/scanner, apg-go, apg-java, apg-cpp, apg-rust, apg-ts, apg-csharp:
tag -> $TAG, revision -> $RELEASE_SHA (the release HEAD), bottle root_url ->
releases/download/$TAG, rebuild bumped. Bottle sha256s + rebuild stay for the
CI bottle rebuild at tag time."

echo "==> Creating annotated tag $TAG -> $RELEASE_SHA"
git tag -a "$TAG" -m "apg $NEW_VERSION" "$RELEASE_SHA"

echo
echo "Done. Release commit: $RELEASE_SHA (tag $TAG)"
echo "Next — human-approved pushes:"
echo "  git push origin main"
echo "  git push origin $TAG"
