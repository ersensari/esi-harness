#!/bin/sh
set -eu

repository_root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
fixture_root=$(mktemp -d "${TMPDIR:-/tmp}/esi-team-smoke.XXXXXX")
hermit_state_dir=${HERMIT_STATE_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/hermit}
rustup_home=${RUSTUP_HOME:-$HOME/.rustup}
cargo_home=${CARGO_HOME:-$repository_root/.hermit/rust}
cargo=${ESI_CARGO:-$repository_root/bin/cargo}
trap 'rm -rf "$fixture_root"' EXIT HUP INT TERM

mkdir -p \
    "$fixture_root/home" \
    "$fixture_root/config" \
    "$fixture_root/cache" \
    "$fixture_root/data" \
    "$fixture_root/tmp"

cd "$repository_root"

HOME="$fixture_root/home" \
XDG_CONFIG_HOME="$fixture_root/config" \
XDG_CACHE_HOME="$fixture_root/cache" \
XDG_DATA_HOME="$fixture_root/data" \
HERMIT_STATE_DIR="$hermit_state_dir" \
RUSTUP_HOME="$rustup_home" \
CARGO_HOME="$cargo_home" \
TMPDIR="$fixture_root/tmp" \
ESI_TEAM_SMOKE_ROOT="$fixture_root" \
ESI_TEAM_SMOKE_NETWORK_ISOLATED="${ESI_TEAM_SMOKE_NETWORK_ISOLATED:-0}" \
FORGELOOP_BASE_URL=http://127.0.0.1:9 \
LITELLM_BASE_URL=http://127.0.0.1:9 \
FORGELOOP_API_KEY= \
LITELLM_API_KEY= \
HTTP_PROXY=http://127.0.0.1:9 \
HTTPS_PROXY=http://127.0.0.1:9 \
ALL_PROXY=http://127.0.0.1:9 \
NO_PROXY= \
"$cargo" test \
    --locked \
    -p esi-development-visualizer \
    --test team_install_smoke \
    -- --nocapture

test ! -e "$fixture_root/home/.codex"
test ! -e "$fixture_root/home/.claude"
test ! -e "$fixture_root/config/goose/secrets.yaml"

echo "ESI clean team local-development smoke: PASS"