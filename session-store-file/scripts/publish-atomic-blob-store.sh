#!/usr/bin/env bash

set -euo pipefail
export STORAGE_RELEASE_PACKAGES="atomic-blob-store"
exec "$(dirname -- "${BASH_SOURCE[0]}")/publish-common.sh" stable "$@"
