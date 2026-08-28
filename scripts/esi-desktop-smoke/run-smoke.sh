#!/bin/sh
set -eu

sandbox=/usr/lib/goose/chrome-sandbox

test "$(stat -c '%U:%G' "$sandbox")" = "root:root"
test "$(stat -c '%a' "$sandbox")" = "4755"

set +e
timeout --signal=TERM 15s \
    dbus-run-session -- \
    xvfb-run -a /usr/lib/goose/Goose --disable-gpu \
    > /tmp/esi-desktop-smoke.log 2>&1
status=$?
set -e

if [ "$status" -ne 124 ]; then
    cat /tmp/esi-desktop-smoke.log
    echo "Desktop exited before the 15-second smoke window (status=$status)" >&2
    exit 1
fi

if grep -q "Running as root without --no-sandbox" /tmp/esi-desktop-smoke.log; then
    cat /tmp/esi-desktop-smoke.log
    echo "Desktop bypassed the required non-root sandbox contract" >&2
    exit 1
fi

if grep -q "SUID sandbox helper binary was found, but is not configured correctly" /tmp/esi-desktop-smoke.log; then
    cat /tmp/esi-desktop-smoke.log
    echo "Desktop Chromium sandbox metadata is invalid" >&2
    exit 1
fi

echo "ESI Desktop normal sandbox smoke: PASS"