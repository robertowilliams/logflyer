#!/usr/bin/env bash
#
# logflayer full dev stack — ONE command runs everything.
#   cd ~/Documents/VECTADB/code/logflayer && ./dev-all.sh
#
# Brings up, in order:
#   1. MongoDB on :27017            (throwaway container if none is running)
#   2. Seeds UpsideGate collections from the bundled fixtures
#   3. API server        → http://localhost:8080   (graph + vector writers on)
#   4. UI dev server     → http://localhost:5173
#
# Logs stream to .scratch/dev-{api,ui}.log (also tailed here).
# Ctrl-C stops everything and removes the throwaway Mongo.

set -uo pipefail
cd "$(dirname "$0")"
mkdir -p .scratch
API_LOG=".scratch/dev-api.log"
UI_LOG=".scratch/dev-ui.log"

# ── Pipeline config (no API keys required) ────────────────────────────────────
export MONGODB_URI="${MONGODB_URI:-mongodb://localhost:27017}"
export API_PORT="${API_PORT:-8080}"
export PREPROCESSING_ENABLED=true
export ENTITY_EXTRACTION_ENABLED=true
export ENTITY_EXTRACTION_MIN_ENTITIES=1
export GRAPH_WRITER_ENABLED=true
export VECTOR_WRITER_ENABLED=true
export EMBEDDING_ENABLED=false
export CLASSIFICATION_ENABLED=false

API_PID=""; UI_PID=""; STARTED_MONGO=0

cleanup() {
  echo; echo "→ Shutting down…"
  [ -n "$UI_PID" ]  && kill "$UI_PID"  2>/dev/null
  [ -n "$API_PID" ] && kill "$API_PID" 2>/dev/null
  [ "$STARTED_MONGO" = "1" ] && { echo "  removing throwaway Mongo"; docker rm -f logflayer-dev-mongo >/dev/null 2>&1; }
  exit 0
}
trap cleanup INT TERM

# ── 1. MongoDB ────────────────────────────────────────────────────────────────
if ! nc -z -w2 localhost 27017 2>/dev/null; then
  command -v docker >/dev/null || { echo "✗ Mongo not on :27017 and docker missing. Start Mongo, re-run." >&2; exit 1; }
  echo "→ Starting throwaway Mongo (mongo:7)…"
  docker rm -f logflayer-dev-mongo >/dev/null 2>&1
  docker run -d -p 27017:27017 --name logflayer-dev-mongo mongo:7 >/dev/null
  STARTED_MONGO=1
  for _ in $(seq 1 20); do nc -z localhost 27017 2>/dev/null && break; sleep 1; done
fi
echo "✓ MongoDB reachable"

# ── 2. Build once + seed ──────────────────────────────────────────────────────
echo "→ Building (first run compiles Rust — may take a few minutes)…"
cargo build --quiet || { echo "✗ cargo build failed" >&2; exit 1; }

echo "→ Seeding UpsideGate collections from fixtures…"
for fx in tests/fixtures/mcp_session.log \
          tests/fixtures/openai_chat_completions.log \
          tests/fixtures/react_agent.log; do
  [ -f "$fx" ] && { echo "   • $fx"; cargo run --quiet -- smoketest "$fx" --target-id dev-seed >/dev/null 2>&1 || echo "     (seed skipped)"; }
done

# ── 3. UI deps ────────────────────────────────────────────────────────────────
if [ ! -d logflayer-ui/node_modules ]; then
  echo "→ Installing UI deps…"
  ( cd logflayer-ui && npm install --no-audit --no-fund )
fi

# ── 4. Start API + UI ─────────────────────────────────────────────────────────
echo "→ Starting API on :${API_PORT}  (log: $API_LOG)"
cargo run >"$API_LOG" 2>&1 &
API_PID=$!

echo "→ Starting UI on :5173  (log: $UI_LOG)"
( cd logflayer-ui && npm run dev ) >"$UI_LOG" 2>&1 &
UI_PID=$!

# Wait for the API health endpoint
for _ in $(seq 1 30); do
  curl -sf "http://localhost:${API_PORT}/health" >/dev/null 2>&1 && break; sleep 1
done

echo
echo "════════════════════════════════════════════════════════"
echo "  API : http://localhost:${API_PORT}"
echo "  UI  : http://localhost:5173"
echo "  seed target_id = dev-seed"
echo "  Ctrl-C to stop everything."
echo "════════════════════════════════════════════════════════"
echo

# Stream both logs until Ctrl-C
tail -f "$API_LOG" "$UI_LOG" &
TAIL_PID=$!
wait "$API_PID" "$UI_PID"
kill "$TAIL_PID" 2>/dev/null
cleanup
