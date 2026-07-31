#!/usr/bin/env bash

set -euo pipefail

PROJECT_ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
EXPECTED=$'p\tShow current directory\tpwd\nl\tList files\tls -la\nd\tShow date and time\tdate\nu\tShow disk use here\tdu -sh .'

cargo build --quiet --manifest-path "$PROJECT_ROOT/Cargo.toml"

ITEMS=$(cd "$PROJECT_ROOT" && ./target/debug/lazymenu-cli --print)
[[ $ITEMS == "$EXPECTED" ]]

if ! command -v script >/dev/null || ! command -v timeout >/dev/null; then
    printf 'Rust parser checks passed; PTY checks skipped (script or timeout unavailable)\n'
    exit 0
fi

TEST_TEMP=$(mktemp -d)
trap 'rm -rf "$TEST_TEMP"' EXIT
RUST_MENU="$PROJECT_ROOT/target/debug/lazymenu-cli"

printf 'q' | timeout 5 script -qfec \
    "stty rows 24 cols 100; env -u NO_COLOR TERM=xterm-256color XDG_STATE_HOME=\"$TEST_TEMP/state\" \"$RUST_MENU\"" \
    /dev/null > "$TEST_TEMP/layout.log"
LAYOUT_HEX=$(od -An -tx1 "$TEST_TEMP/layout.log" | tr -d ' \n')
rg -q '1b5b313b3148' <<< "$LAYOUT_HEX"
rg -q '1b5b353b3148' <<< "$LAYOUT_HEX"
rg -q '1b5b34383b353b31346d' <<< "$LAYOUT_HEX"
rg -q '1b5b3f3130343968' <<< "$LAYOUT_HEX"
rg -q '1b5b3f313034396c' <<< "$LAYOUT_HEX"
rg -Fq 'p) Show current directory' "$TEST_TEMP/layout.log"

{ sleep 0.15; printf '\033'; } | timeout 5 script -qfec \
    "stty rows 24 cols 100; XDG_STATE_HOME=\"$TEST_TEMP/state\" \"$RUST_MENU\"" \
    /dev/null > "$TEST_TEMP/escape.log"

{
    sleep 0.15
    printf '/clock'
    sleep 0.15
    printf '\r'
    sleep 0.15
    printf ' '
    sleep 0.15
    printf '\033'
    sleep 0.08
    printf '\033'
    sleep 0.08
    printf '\033'
} | timeout 5 script -qfec \
    "cd \"$PROJECT_ROOT\" && stty rows 24 cols 100 && XDG_STATE_HOME=\"$TEST_TEMP/state\" \"$RUST_MENU\"" \
    /dev/null > "$TEST_TEMP/search.log"
rg -q 'Search: clock' "$TEST_TEMP/search.log"
rg -q 'Running: date' "$TEST_TEMP/search.log"
rg -q '^id:show-date$' "$TEST_TEMP/state/lazymenu-cli/recent-items"

{
    sleep 0.3
    printf 'x'
    sleep 0.5
    printf '\003'
    sleep 0.5
    printf 'q'
} | timeout 5 script -qfec \
    "stty rows 24 cols 100; XDG_STATE_HOME=\"$TEST_TEMP/state\" \"$RUST_MENU\" --config \"$PROJECT_ROOT/tests/interrupt-menu.toml\"" \
    /dev/null > "$TEST_TEMP/interrupt.log"
rg -q 'interrupted' "$TEST_TEMP/interrupt.log"

printf 'x' | timeout 5 script -qfec \
    "stty rows 24 cols 100; XDG_STATE_HOME=\"$TEST_TEMP/state\" \"$RUST_MENU\" --config \"$PROJECT_ROOT/tests/exit-after-command.toml\"" \
    /dev/null > "$TEST_TEMP/exit-after-command.log"
rg -q 'Running: echo exit-once' "$TEST_TEMP/exit-after-command.log"
rg -q '^id:exit-once$' "$TEST_TEMP/state/lazymenu-cli/recent-items"
if rg -q 'Press any key to return' "$TEST_TEMP/exit-after-command.log"; then
    printf 'Non-looping menu unexpectedly prompted to return\n' >&2
    exit 1
fi

set +e
printf 'f' | timeout 5 script -qfec \
    "stty rows 24 cols 100; XDG_STATE_HOME=\"$TEST_TEMP/state\" \"$RUST_MENU\" --config \"$PROJECT_ROOT/tests/exit-after-command.toml\"" \
    /dev/null > "$TEST_TEMP/exit-status.log"
EXIT_STATUS=$?
set -e
[[ $EXIT_STATUS -eq 7 ]]

printf 'Rust layout, search, XDG recency, Ctrl+C, and menu-loop checks passed\n'
