#!/usr/bin/env bash

set -euo pipefail
export STORAGE_RELEASE_PACKAGES="rumqttc-session-store-file-next"
exec "$(dirname -- "${BASH_SOURCE[0]}")/publish-common.sh" prerelease "$@"
