#!/usr/bin/env bash
#
# UpsideGate verification gate — run on a machine with cargo + docker.
# Usage:  cd ~/Documents/VECTADB/logflayer && ./verify.sh
#
# Steps: cargo check → cargo test --lib → (optional) local Mongo + smoke test.
# Any failure stops the script. Output is also saved to .scratch/verify-*.log.

set -uo pipefail
cd "$(dirname "$0")"
mkdir -p .scratch
STAMP="$(date +%Y%m%d-%H%M%S)"

run() { echo; echo "=== $1 ==="; }

# 1. Compile check
run "cargo check"
cargo check 2>&1 | tee ".scratch/verify-check-${STAMP}.log"
[ "${PIPESTATUS[0]}" -ne 0 ] && { echo "FAILED: cargo check"; exit 1; }

# 2. Library tests
run "cargo test --lib"
cargo test --lib 2>&1 | tee ".scratch/verify-test-${STAMP}.log"
[ "${PIPESTATUS[0]}" -ne 0 ] && { echo "FAILED: cargo test --lib"; exit 1; }

# 3. Smoke test (needs a reachable Mongo)
export MONGODB_URI="${MONGODB_URI:-mongodb://localhost:27017}"
export ENTITY_EXTRACTION_ENABLED=true
export GRAPH_WRITER_ENABLED=true
export VECTOR_WRITER_ENABLED=true
export EMBEDDING_ENABLED=false

if ! nc -z -w2 localhost 27017 2>/dev/null && command -v docker >/dev/null; then
  echo; echo "No Mongo on :27017 — starting a throwaway container (mongo:7)…"
  docker rm -f logflayer-smoke-mongo >/dev/null 2>&1
  docker run -d -p 27017:27017 --name logflayer-smoke-mongo mongo:7 >/dev/null
  for i in $(seq 1 20); do nc -z localhost 27017 2>/dev/null && break; sleep 1; done
  STARTED_MONGO=1
fi

run "cargo run -- smoketest tests/fixtures/mcp_session.log"
cargo run -- smoketest tests/fixtures/mcp_session.log 2>&1 | tee ".scratch/verify-smoke-${STAMP}.log"
SMOKE_RC="${PIPESTATUS[0]}"

[ "${STARTED_MONGO:-0}" = "1" ] && { echo "Removing throwaway Mongo…"; docker rm -f logflayer-smoke-mongo >/dev/null 2>&1; }

[ "$SMOKE_RC" -ne 0 ] && { echo "FAILED: smoke test"; exit 1; }

echo; echo "✅ All gates passed. Logs in .scratch/verify-*-${STAMP}.log"
