#!/usr/bin/env bash
#
# test_logflayer.sh — one-shot test harness for the logflayer UpsideGate pipeline.
#
# What it does, in order:
#   1. Checks prerequisites (cargo; docker/mongosh optional)
#   2. Ensures a MongoDB is reachable (starts a throwaway `mongo:7` container if not)
#   3. Sets the UpsideGate env vars (entity extraction + graph + vector writers ON)
#   4. Verification gate:  cargo check  +  cargo test --lib
#   5. End-to-end smoke test on a fixture (default: mcp_session.log)
#   6. Verifies the resulting MongoDB collection counts
#
# Usage:
#   ./test_logflayer.sh                         # full run, default fixture
#   ./test_logflayer.sh langchain_json.log      # use a different fixture
#   ./test_logflayer.sh --skip-tests            # skip cargo check/test, just smoke-test
#   ./test_logflayer.sh --fresh                 # wipe smoketest data first
#   ./test_logflayer.sh --no-mongo              # don't try to start a Mongo container
#   ./test_logflayer.sh --serve                 # after smoke test, start the API on :8080
#
# Env overrides (anything in .env or your shell wins):
#   MONGODB_URI   (read from .env; currently a MongoDB Atlas SRV URI.
#                  A remote/Atlas URI never triggers the local Docker-Mongo path.)
#   EMBEDDING_ENABLED=true EMBEDDING_API_KEY=...   # to also test content embeddings

set -euo pipefail

# ── locate ourselves (works regardless of where it's invoked from) ────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

# ── pretty output ─────────────────────────────────────────────────────────────
if [[ -t 1 ]]; then
  BOLD=$'\033[1m'; RED=$'\033[31m'; GRN=$'\033[32m'; YLW=$'\033[33m'; CYN=$'\033[36m'; RST=$'\033[0m'
else
  BOLD=""; RED=""; GRN=""; YLW=""; CYN=""; RST=""
fi
step() { echo; echo "${BOLD}${CYN}── $* ${RST}"; }
ok()   { echo "${GRN}✓${RST} $*"; }
warn() { echo "${YLW}!${RST} $*"; }
die()  { echo "${RED}✗ $*${RST}" >&2; exit 1; }

# ── args ──────────────────────────────────────────────────────────────────────
FIXTURE="mcp_session.log"
SKIP_TESTS=0
FRESH=0
NO_MONGO=0
SERVE=0
for arg in "$@"; do
  case "$arg" in
    --skip-tests) SKIP_TESTS=1 ;;
    --fresh)      FRESH=1 ;;
    --no-mongo)   NO_MONGO=1 ;;
    --serve)      SERVE=1 ;;
    --help|-h)    sed -n '2,30p' "$0"; exit 0 ;;
    -*)           die "unknown flag: $arg" ;;
    *)            FIXTURE="$arg" ;;
  esac
done

FIXTURE_PATH="tests/fixtures/${FIXTURE#tests/fixtures/}"   # accept bare name or full path

# ── 0. sanity ─────────────────────────────────────────────────────────────────
step "Prerequisites"
command -v cargo >/dev/null 2>&1 || die "cargo not found — install the Rust toolchain (https://rustup.rs)"
ok "cargo $(cargo --version | awk '{print $2}')"
[[ -f "$FIXTURE_PATH" ]] || die "fixture not found: $FIXTURE_PATH (try: ls tests/fixtures)"
ok "fixture: $FIXTURE_PATH"

HAVE_MONGOSH=0; command -v mongosh >/dev/null 2>&1 && HAVE_MONGOSH=1
HAVE_DOCKER=0;  command -v docker  >/dev/null 2>&1 && HAVE_DOCKER=1

# ── 1. environment ────────────────────────────────────────────────────────────
step "Environment"
# Load .env if present (lets you keep API keys out of this script).
if [[ -f .env ]]; then set -a; source .env; set +a; ok "loaded .env"; fi
export MONGODB_URI="${MONGODB_URI:-mongodb://localhost:27017}"
# Normalised base (no trailing slash) + target DB, for building mongosh connect
# strings.  Avoids the double-slash that breaks SRV URIs ending in '/'.
MONGODB_BASE="${MONGODB_URI%/}"
MONGO_DB_NAME="${DESTINATION_DB_NAME:-log_samples}"
export ENTITY_EXTRACTION_ENABLED="${ENTITY_EXTRACTION_ENABLED:-true}"
export GRAPH_WRITER_ENABLED="${GRAPH_WRITER_ENABLED:-true}"
export VECTOR_WRITER_ENABLED="${VECTOR_WRITER_ENABLED:-true}"
export EMBEDDING_ENABLED="${EMBEDDING_ENABLED:-false}"
echo "  MONGODB_URI                = $MONGODB_URI"
echo "  ENTITY_EXTRACTION_ENABLED  = $ENTITY_EXTRACTION_ENABLED"
echo "  GRAPH_WRITER_ENABLED       = $GRAPH_WRITER_ENABLED"
echo "  VECTOR_WRITER_ENABLED      = $VECTOR_WRITER_ENABLED"
echo "  EMBEDDING_ENABLED          = $EMBEDDING_ENABLED"
[[ "$EMBEDDING_ENABLED" == "true" && -z "${EMBEDDING_API_KEY:-}" ]] && \
  warn "EMBEDDING_ENABLED=true but EMBEDDING_API_KEY is empty — content embeddings will fail (behavioral still work)"

# ── 2. MongoDB ────────────────────────────────────────────────────────────────
step "MongoDB"
# Remote (Atlas / SRV / any non-localhost) vs. local decides whether we may
# spin up a throwaway container.  We never start a local Mongo for a remote URI.
IS_REMOTE=0
case "$MONGODB_URI" in
  mongodb+srv://*) IS_REMOTE=1 ;;                       # Atlas SRV
  *@localhost*|*@127.0.0.1*|mongodb://localhost*|mongodb://127.0.0.1*) IS_REMOTE=0 ;;
  *) IS_REMOTE=1 ;;                                      # any other host
esac

mongo_up() {
  # Only mongosh can validate an SRV/auth URI; raw TCP only makes sense locally.
  if [[ $HAVE_MONGOSH -eq 1 ]]; then
    mongosh "$MONGODB_URI" --quiet --eval 'db.runCommand({ping:1}).ok' 2>/dev/null | grep -q 1
  elif [[ $IS_REMOTE -eq 0 ]]; then
    (exec 3<>/dev/tcp/localhost/27017) 2>/dev/null && exec 3>&- 3<&-
  else
    return 2   # remote + no mongosh: can't verify, caller decides
  fi
}

if [[ $IS_REMOTE -eq 1 ]]; then
  # Atlas / remote: assume it's up; verify with mongosh if we have it.
  host="${MONGODB_URI#*@}"; host="${host%%/*}"
  if mongo_up; then
    ok "MongoDB Atlas reachable ($host)"
  elif [[ $? -eq 2 ]]; then
    warn "Using remote MongoDB ($host) — mongosh not installed, can't pre-verify (cargo will connect at runtime)"
  else
    die "Remote MongoDB ($host) not reachable — check the URI, your network, and the Atlas IP allowlist (add 0.0.0.0/0 or your IP)"
  fi
elif mongo_up; then
  ok "MongoDB reachable at $MONGODB_URI"
elif [[ $NO_MONGO -eq 1 ]]; then
  die "MongoDB not reachable and --no-mongo was passed"
elif [[ $HAVE_DOCKER -eq 1 ]]; then
  warn "MongoDB not reachable — starting a throwaway 'mongo:7' container"
  docker rm -f logflayer-test-mongo >/dev/null 2>&1 || true
  docker run -d -p 27017:27017 --name logflayer-test-mongo mongo:7 >/dev/null
  echo -n "  waiting for Mongo to accept connections"
  for _ in $(seq 1 30); do echo -n "."; sleep 1; mongo_up && break; done; echo
  mongo_up || die "Mongo container did not become ready in time"
  ok "started container 'logflayer-test-mongo' (remove later with: docker rm -f logflayer-test-mongo)"
else
  die "MongoDB not reachable and neither mongosh nor docker is available to start one"
fi

# ── optional: wipe prior smoke-test data ──────────────────────────────────────
if [[ $FRESH -eq 1 && $HAVE_MONGOSH -eq 1 ]]; then
  step "Resetting smoke-test data (--fresh)"
  mongosh "$MONGODB_BASE/$MONGO_DB_NAME" --quiet --eval '
    db.sample_metadata.deleteMany({target_id:"smoketest"});
    db.entity_edges.deleteMany({}); db.prov_relations.deleteMany({});
    db.otel_spans.deleteMany({}); db.content_embeddings.deleteMany({});
    db.behavioral_embeddings.deleteMany({}); db.smoketest.drop();' >/dev/null
  ok "cleared"
fi

# ── 3. verification gate ──────────────────────────────────────────────────────
if [[ $SKIP_TESTS -eq 0 ]]; then
  step "cargo check"
  cargo check 2>&1 | tee /tmp/logflayer-cargo-check.log
  ok "cargo check passed (log: /tmp/logflayer-cargo-check.log)"

  step "cargo test --lib"
  cargo test --lib 2>&1 | tee /tmp/logflayer-cargo-test.log
  ok "cargo test passed (log: /tmp/logflayer-cargo-test.log)"
else
  warn "skipping cargo check / test (--skip-tests)"
fi

# ── 4. smoke test ─────────────────────────────────────────────────────────────
step "Smoke test — $FIXTURE_PATH"
cargo run -- smoketest "$FIXTURE_PATH"

# ── 5. verify collection counts ───────────────────────────────────────────────
if [[ $HAVE_MONGOSH -eq 1 ]]; then
  step "MongoDB collection counts ($MONGO_DB_NAME)"
  mongosh "$MONGODB_BASE/$MONGO_DB_NAME" --quiet --eval '
    const c = n => db.getCollection(n).countDocuments();
    print("  sample_metadata:      " + c("sample_metadata"));
    print("  entity_edges:         " + c("entity_edges"));
    print("  prov_relations:       " + c("prov_relations"));
    print("  otel_spans:           " + c("otel_spans"));
    print("  content_embeddings:   " + c("content_embeddings"));
    print("  behavioral_embeddings:" + c("behavioral_embeddings"));'
  echo
  ok "If entity_edges / prov_relations / otel_spans are > 0, the full pipeline persisted correctly."
else
  warn "mongosh not installed — skipping count verification (the smoke-test output above already reports them)"
fi

# ── 6. optional: serve the API ────────────────────────────────────────────────
if [[ $SERVE -eq 1 ]]; then
  step "Starting API on :${API_PORT:-8080} (Ctrl-C to stop)"
  echo "  Try, in another terminal:"
  echo "    curl 'http://localhost:${API_PORT:-8080}/api/v1/relations?limit=5' | jq"
  echo "    curl 'http://localhost:${API_PORT:-8080}/api/v1/prov?limit=5'      | jq"
  echo "    curl 'http://localhost:${API_PORT:-8080}/api/v1/spans?limit=5'     | jq"
  cargo run
fi

step "Done"
ok "Smoke test complete."
