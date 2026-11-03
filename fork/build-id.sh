#!/bin/bash
# fork: build id for fork builds: the commit's UTC time, then its short sha.
# The time prefix makes ids sort by commit time, which `herdr update` relies on
# to offer only newer builds. Used by fork/install.sh and fork-release.yml.
set -euo pipefail
cd "$(dirname "$0")/.."
echo "$(TZ=UTC git show -s --format=%cd --date=format-local:%Y%m%d%H%M%S HEAD)-$(git rev-parse HEAD | cut -c1-7)"
