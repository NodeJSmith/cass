#!/usr/bin/env bash
# scripts/e2e/doctor_v2.sh
# Scripted cass doctor v2 E2E runner. The Rust runner creates isolated
# scenario roots and durable artifacts under test-results/e2e/doctor-v2/.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"

LABELS="quick"
SCENARIOS=""
NO_BUILD=0
INCLUDE_FAILURE_SELF_TEST=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --label|--labels)
      LABELS="${2:?--label requires a comma-separated value}"
      shift 2
      ;;
    --scenario|--scenarios)
      SCENARIOS="${2:?--scenario requires a comma-separated value}"
      shift 2
      ;;
    --no-build)
      NO_BUILD=1
      shift
      ;;
    --include-failure-self-test)
      INCLUDE_FAILURE_SELF_TEST=1
      shift
      ;;
    --help|-h)
      cat <<'USAGE'
Usage: scripts/e2e/doctor_v2.sh [--label quick,fault] [--scenario quick-source-pruned] [--include-failure-self-test] [--no-build]

Artifacts:
  test-results/e2e/doctor-v2/run-*/artifacts/<scenario>/

The runner only invokes robot-safe cass commands. It never launches bare cass.
USAGE
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
done

cd "$PROJECT_ROOT"

RUN_ID="run-$(date -u +%Y%m%dT%H%M%SZ)-$$"
RUN_ROOT="${PROJECT_ROOT}/test-results/e2e/doctor-v2/${RUN_ID}"

if [[ "$NO_BUILD" -eq 0 ]]; then
  cargo build --bin cass
fi

export CASS_DOCTOR_E2E_LABELS="$LABELS"
export CASS_DOCTOR_E2E_SCENARIOS="$SCENARIOS"
export CASS_DOCTOR_E2E_RUN_ROOT="$RUN_ROOT"
if [[ "$INCLUDE_FAILURE_SELF_TEST" -eq 1 ]]; then
  export CASS_DOCTOR_E2E_INCLUDE_FAILURE_SELF_TEST=1
fi

cargo test --test doctor_e2e_runner doctor_e2e_scripted_scenarios -- --nocapture

echo "Artifacts: ${RUN_ROOT}"
