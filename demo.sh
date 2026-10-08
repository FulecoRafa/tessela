#!/usr/bin/env bash
# Builds everything, starts the Go and Python services, runs the bar.
# Extra arguments go to marathon, e.g.:
#   ./demo.sh                                  # terminal bar (interactive)
#   ./demo.sh --headless --seconds 8 --click profile@3
#   ./demo.sh --gpui                           # needs: cargo build --features gpui
set -euo pipefail
cd "$(dirname "$0")"

export MARATHON_SERVICES_DIR="$PWD/run/services"
mkdir -p "$MARATHON_SERVICES_DIR" target
chmod 700 run "$MARATHON_SERVICES_DIR"

features=()
for a in "$@"; do [[ "$a" == "--gpui" ]] && features=(--features gpui); done
cargo build -q -p shell "${features[@]}"
(cd services/power && go build -o ../../target/power .)

pids=()
cleanup() { kill "${pids[@]}" 2>/dev/null || true; wait 2>/dev/null || true; }
trap cleanup EXIT

./target/power 2>run/power.log & pids+=($!)
python3 services/sys/sys_service.py 2>run/sys.log & pids+=($!)

./target/debug/marathon --config marathon.toml "$@"
