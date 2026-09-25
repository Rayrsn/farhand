#!/usr/bin/env bash
# Run the criterion benchmark suites and regenerate BENCHMARKS.md.
#
# Usage:
#   scripts/run_benchmarks.sh            # full-quality run (criterion defaults)
#   scripts/run_benchmarks.sh --fast     # quick run for local iteration
set -euo pipefail
cd "$(dirname "$0")/.."

FAST_ARGS=(--warm-up-time 0.5 --measurement-time 1 --sample-size 20)

if [ "${1:-}" = "--fast" ]; then
    cargo bench --bench sync -- "${FAST_ARGS[@]}"
    cargo bench --bench cas_cow -- "${FAST_ARGS[@]}"
else
    cargo bench --bench sync
    cargo bench --bench cas_cow
fi

python3 scripts/render_benchmarks.py > BENCHMARKS.md
echo "BENCHMARKS.md regenerated."