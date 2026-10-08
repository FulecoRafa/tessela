#!/usr/bin/env bash
# Everything that can be verified without a display: unit tests (including the
# sandbox escape tests), Go vet, and an end-to-end headless run with clicks.
set -euo pipefail
cd "$(dirname "$0")"

echo "== cargo test"
cargo test -q 2>&1 | grep -E "^test result|FAILED|panicked" || true
cargo test -q >/dev/null 2>&1

echo "== go vet"
(cd services/power && go vet ./...)
(cd sdk/go && go vet ./...)

echo "== python"
python3 -m py_compile sdk/python/marathon_service.py services/sys/sys_service.py

echo "== end to end (headless, 7s)"
out=$(./demo.sh --headless --seconds 7 --click profile@2.5 --click clock@4 2>&1)
echo "$out" | tail -4
grep -q "desempenho" <<<"$out" || { echo "FAIL: profile click did not reach the Go service"; exit 1; }
grep -Eq "\[\*◷ [0-9]{2}:[0-9]{2}:[0-9]{2}\]" <<<"$out" || { echo "FAIL: clock click did not toggle seconds"; exit 1; }
grep -Eq "[0-9]+%~?\]" <<<"$out" || { echo "FAIL: battery never rendered"; exit 1; }
grep -q "/[0-9]*G" <<<"$out" || { echo "FAIL: Python sys service never rendered"; exit 1; }
echo "OK"
