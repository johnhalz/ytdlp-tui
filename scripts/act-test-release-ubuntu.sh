#!/usr/bin/env bash
# Run the release workflow locally for the Linux matrix job only (act has no macOS/Windows runners).
# Requires Docker. The final "Upload to GitHub Release" step needs a token or will fail; PyInstaller still runs first.
#
# Usage: ./scripts/act-test-release-ubuntu.sh
# Optional: act args, e.g. ./scripts/act-test-release-ubuntu.sh --secret GITHUB_TOKEN

set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

exec act push \
  -W .github/workflows/release.yml \
  -e .github/act/tag-push.json \
  --matrix os:ubuntu-latest \
  --container-architecture linux/amd64 \
  -P ubuntu-latest=catthehacker/ubuntu:act-22.04 \
  "$@"
