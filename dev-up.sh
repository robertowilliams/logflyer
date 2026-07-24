#!/usr/bin/env bash
#
# logflayer dev stack — real backend with seeded data.
# Run on your Mac:  cd ~/Documents/VECTADB/code/logflayer && ./dev-up.sh
#
# What it does:
#   1. Ensures MongoDB is reachable on :27017 (starts a throwaway container if not).
#   2. Seeds the UpsideGate collections from the bundled fixtures, so the
#      Entities / Relations / PROV / Spans views have real content on first load.
#   3. Starts the API server on :8080 with the graph + vector writers enabled.
#
# Then, in a SECOND terminal, start the UI:
#   cd ~/Documents/VECTADB/code/logflayer/logflayer-ui && npm install && npm run dev
# and open the URL it prints (http://localhost:5173).
#
# Stop: Ctrl-C here. Remove the throwaway Mongo with:  docker rm -f logflayer-dev-mongo

set -uo pipefail
cd "$(dirname "$0")"

# ── Pipeline configuration (UpsideGate on; LLM classification + content
#    embeddings stay off so no API keys are required) ──────────────────────────
export MONGODB_URI="${MONGODB_URI:-mongodb://localhost:27017}"
export API_PORT="${API_PORT:-8080}"
export PREPROCESSING_ENABLED=true
export ENTITY_EXTRACTION_ENABLED=true
export ENTITY_EXTRACTION_MIN_ENTITIES=1
export GRAPH_WRITER_ENABLED=true
export VECTOR_WRITER_ENABLED=true
export EMBEDDING_ENABLED=false
export CLASSIFICATION_ENABLED=false

# ── 1. MongoDB ────────────────────────────────────────────────────────────────
if ! nc -z -w2 localhost 27017 2>/dev/null; then
  if command -v docker >/dev/null; then
    echo "→ No Mongo on :27017 — starting throwaway container (mongo:7)…"
    docker rm -f logflayer-dev-mongo >/dev/null 2>&1
    docker run -d -p 27017:27017 --name logflayer-dev-mongo mongo:7 >/dev/null
    for _ in $(seq 1 20); do nc -z localhost 27017 2>/dev/null && break; sleep 1; done
  else
    echo "✗ MongoDB not reachable on :27017 and docker not found. Start Mongo, then re-run." >&2
    exit 1
  fi
fi
echo "✓ MongoDB reachable on :27017"

# ── 2. Seed data from bundled fixtures ────────────────────────────────────────
echo "→ Seeding UpsideGate collections from fixtures…"
for fx in tests/fixtures/mcp_session.log \
          tests/fixtures/openai_chat_completions.log \
          tests/fixtures/react_agent.log; do
  [ -f "$fx" ] || continue
  echo "   • $fx"
  cargo run --quiet -- smoketest "$fx" --target-id dev-seed >/dev/null 2>&1 \
    && echo "     seeded" || echo "     (seed skipped — check 'cargo run -- smoketest $fx')"
done

# ── 3. API server ─────────────────────────────────────────────────────────────
echo
echo "→ Starting logflayer API on :${API_PORT}  (Ctrl-C to stop)"
echo "  Next: in another terminal → cd logflayer-ui && npm run dev → open http://localhost:5173"
echo
exec cargo run
