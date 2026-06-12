#!/usr/bin/env bash
# Set GitHub repository "About" metadata for Z0rlord/minervamnt.
# Usage: doppler run --project minervamnt --config dev -- bash scripts/update-github-about.sh

set -euo pipefail

REPO="${GITHUB_REPO:-Z0rlord/minervamnt}"

gh repo edit "$REPO" \
  --description "Open-source Ark-backed Cashu mint in Rust — Chaumian ecash backed by VTXOs with PoL/PoR transparency. Experimental scaffold." \
  --homepage "https://minervamnt.xyz" \
  --enable-issues \
  --add-topic cashu \
  --add-topic bitcoin \
  --add-topic ecash \
  --add-topic ark \
  --add-topic rust \
  --add-topic chaumian \
  --add-topic privacy \
  --add-topic open-source \
  --add-topic vtxo \
  --add-topic proof-of-liabilities

echo "Updated About for $REPO:"
gh repo view "$REPO" --json description,homepageUrl,repositoryTopics,hasIssuesEnabled,licenseInfo \
  --jq '{description, homepage: .homepageUrl, topics: [.repositoryTopics[].name], issues: .hasIssuesEnabled, license: .licenseInfo.spdxId}'
