#!/usr/bin/env bash
# Run `cargo test` with the given arguments, and fail when no test ran.
# A name filter that matches nothing otherwise passes silently, which
# would leave a nightly step green while it tested nothing.
set -euo pipefail

log=$(mktemp)
trap 'rm -f "$log"' EXIT

cargo test "$@" 2>&1 | tee "$log"

passed=$(awk '/^test result:/ { n += $4 } END { print n + 0 }' "$log")
if [ "$passed" -eq 0 ]; then
  echo "::error::cargo test $* ran no test" >&2
  exit 1
fi
