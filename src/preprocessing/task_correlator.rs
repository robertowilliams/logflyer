//! Stage 11 — group entities into **tasks**.
//!
//! Every other identifier in the pipeline is scoped to a sample: `trace_id`,
//! `span_id`, `entity_id` and `relation_id` all derive from `sample_hash`. That
//! makes the graph boundary whatever the log sampler happened to cut, which is an
//! artifact of collection rather than anything meaningful. A task spanning two
//! samples becomes two disconnected graphs; two tasks landing in one sample merge
//! into one.
//!
//! This module derives a `task_id` from a **correlation key found in the log
//! itself** — `session_id`, `run_id`, `crew_id` and friends — so the same task
//! reassembles across however many samples it was collected in. That is the unit
//! an audit actually asks about: "why did the agent decide this, on this job".
//!
//! # Why `trace_id` was not simply redefined
//!
//! `trace_id` is load-bearing where changing it would corrupt existing data:
//! `otel_spans` upserts on `(trace_id, span_id)`, and `PART_OF` edges target the
//! trace id and feed it into `derive_relation_id`. Re-keying it would orphan
//! records in both collections, and `purge_stale_metadata` does not reach either.
//! So `task_id` is **additive** and `trace_id` keeps its OTel meaning of one
//! execution. See `docs/plan-task-semantic-db.md` §2.
//!
//! # Coverage is honest, not universal
//!
//! Only some log formats carry a correlation key at all — of the bundled fixtures,
//! langchain, crewai, bedrock and react do; the MCP, OpenAI and nginx ones do not.
//! Where none exists the task falls back to sample scope, which is exactly today's
//! behaviour and no worse. [`TaskCorrelation::source`] records which happened, so a
//! consumer can tell a real task boundary from a fallback rather than trusting all
//! of them equally.

use std::collections::HashMap;

use serde_json::Value;

use super::{entity_extractor, ids};
use crate::models::EntityRecord;

/// Field names that identify a task, **most specific first**.
///
/// Order matters and is the module's main policy decision. The reasoning:
///
/// * `task_id` — a log that names its own task is authoritative; nothing beats it.
/// * `run_id` — one execution of an agent, which is what a task usually means.
/// * `session_id` — a conversation. Coarser than a run: one session can contain
///   several tasks, so it loses if a run id is also present.
/// * `conversation_id` / `thread_id` — the same idea under other vendors' names.
/// * `crew_id` — CrewAI's multi-agent grouping. Coarser still, so it ranks below
///   the per-run keys.
/// * `request_id` — finest granularity and often *narrower* than a task (one HTTP
///   call), so it is the last resort before falling back to the sample. Better
///   than nothing because it at least groups a request's own lines.
const CORRELATION_KEYS: &[&str] = &[
    "task_id",
    "run_id",
    "session_id",
    "conversation_id",
    "thread_id",
    "crew_id",
    "request_id",
];

/// Marker used as the key name when no correlation key was found.
pub const SAMPLE_FALLBACK: &str = "sample";

/// The outcome of correlating one sample.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskCorrelation {
    /// Stable 32-hex-char task id. Shared across samples when they share a key.
    pub task_id: String,
    /// Which field the id came from — one of [`CORRELATION_KEYS`], or
    /// [`SAMPLE_FALLBACK`] when the log carried none.
    ///
    /// Present so an audit trail does not overstate its own confidence: a task
    /// boundary derived from `session_id` means something, one derived from the
    /// sample hash only means "we could not tell".
    pub source: String,
    /// The raw value the id was derived from, for display and debugging.
    /// `None` for the sample fallback, where the value is just the sample hash.
    pub correlation_key: Option<String>,
}

impl TaskCorrelation {
    /// Whether this task boundary came from the log rather than from the sampler.
    pub fn is_real_boundary(&self) -> bool {
        self.source != SAMPLE_FALLBACK
    }
}

/// Derive the task correlation for a sample by scanning **every line** of it.
///
/// # Why the whole sample and not just the entities
///
/// Correlation keys routinely appear on lines that never become entities. In
/// `crewai_logfmt.log` the `task_id` is on `msg="Task assigned"` lines, which match
/// no entity-type pattern at all — an earlier version of this function read
/// `entities[].extracted_fields` and consequently fell back to sample scope on
/// that fixture despite the key being right there in the log.
///
/// A correlation key describes the *sample's* provenance, not any individual
/// event's, so it belongs with the other whole-content stages (format detection,
/// stats) rather than with entity extraction. Scanning independently also means
/// task correlation still works when entity extraction is switched off.
///
/// Precedence is applied **globally**: the highest-ranked key is looked for across
/// all lines before the next is considered, so a coarse key on line 1 cannot beat
/// a finer one on line 12.
///
/// Falls back to `("sample", sample_hash)` when no line carries a known key.
pub fn correlate(content: &str, is_json: bool, sample_hash: &str) -> TaskCorrelation {
    // Parse each line's fields once, then scan by key. Reuses the entity
    // extractor's parsers rather than adding a third copy of field parsing.
    let per_line: Vec<HashMap<String, Value>> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            if is_json {
                entity_extractor::extract_json_fields(line.trim())
            } else {
                entity_extractor::extract_logfmt_fields(line.trim())
            }
        })
        .collect();

    for key in CORRELATION_KEYS {
        if let Some(value) = per_line.iter().filter_map(|f| extract_key(f, key)).next() {
            return TaskCorrelation {
                task_id: ids::derive_task_id(key, &value),
                source: (*key).to_string(),
                correlation_key: Some(value),
            };
        }
    }

    TaskCorrelation {
        task_id: ids::derive_task_id(SAMPLE_FALLBACK, sample_hash),
        source: SAMPLE_FALLBACK.to_string(),
        correlation_key: None,
    }
}

/// Read a correlation value out of one entity's extracted fields.
///
/// Accepts strings and numbers, because logfmt stringifies everything while JSON
/// logs sometimes use a numeric run counter. Anything blank, or a placeholder like
/// `null` / `none`, is treated as absent — an empty correlation key would merge
/// every sample that shares the emptiness into one enormous task, which is far
/// worse than falling back.
fn extract_key(fields: &HashMap<String, Value>, key: &str) -> Option<String> {
    let raw = match fields.get(key)? {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };

    if raw.is_empty() || PLACEHOLDERS.contains(&raw.to_ascii_lowercase().as_str()) {
        return None;
    }
    Some(raw)
}

/// Values that mean "no correlation key" despite being present.
const PLACEHOLDERS: &[&str] = &["null", "nil", "<nil>", "none", "-", "n/a", "unknown", "0"];

/// Stamp a correlation onto every entity in a sample.
///
/// Applied in place after extraction, so entities carry their task id when they
/// are written into `sample_metadata.entities`.
pub fn apply(entities: &mut [EntityRecord], correlation: &TaskCorrelation) {
    for entity in entities.iter_mut() {
        entity.task_id = correlation.task_id.clone();
        entity.correlation_key = correlation.correlation_key.clone();
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{EntityType, SemanticRole};

    /// Build a single JSON log line carrying `fields`.
    fn json_line(fields: serde_json::Value) -> String {
        fields.to_string()
    }

    /// Correlate a single JSON line.
    fn correlate_json(fields: serde_json::Value) -> TaskCorrelation {
        correlate(&json_line(fields), true, "h")
    }

    /// Correlate several JSON lines, in order.
    fn correlate_lines(lines: &[serde_json::Value]) -> TaskCorrelation {
        let content = lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n");
        correlate(&content, true, "h")
    }

    fn entity_with(fields: serde_json::Value) -> EntityRecord {
        let mut e = EntityRecord {
            entity_id: "eid".to_string(),
            entity_type: EntityType::PromptEvent,
            semantic_role: SemanticRole::Unknown,
            sample_hash: "testhash".to_string(),
            target_id: "t".to_string(),
            trace_id: "trace".to_string(),
            span_id: "span".to_string(),
            parent_span_id: None,
            prov_entity_id: "ug:entity:eid".to_string(),
            prov_activity_id: "ug:activity:testhash:0".to_string(),
            line_index: 0,
            raw_text: String::new(),
            extracted_fields: HashMap::new(),
            model_id: None,
            tool_name: None,
            mcp_server_id: None,
            token_count: None,
            latency_ms: None,
            timestamp_utc: None,
            content_embedding_id: None,
            behavioral_embedding_id: None,
            task_id: String::new(),
            correlation_key: None,
        };
        if let Some(obj) = fields.as_object() {
            e.extracted_fields = obj.clone().into_iter().collect();
        }
        e
    }

    fn plain() -> EntityRecord {
        entity_with(serde_json::json!({}))
    }

    // ── Precedence ───────────────────────────────────────────────────────────

    #[test]
    fn task_id_beats_everything() {
        let c = correlate_json(serde_json::json!({
            "task_id": "T1", "run_id": "R1", "session_id": "S1", "crew_id": "C1",
        }));
        assert_eq!(c.source, "task_id");
        assert_eq!(c.correlation_key.as_deref(), Some("T1"));
    }

    #[test]
    fn run_id_beats_session_id() {
        // A session can contain several runs, so the run is the finer — and
        // therefore more task-like — boundary.
        assert_eq!(correlate_json(serde_json::json!({ "session_id": "S1", "run_id": "R1" })).source, "run_id");
    }

    #[test]
    fn session_id_beats_crew_id() {
        assert_eq!(correlate_json(serde_json::json!({ "crew_id": "C1", "session_id": "S1" })).source, "session_id");
    }

    #[test]
    fn crew_id_beats_request_id() {
        assert_eq!(correlate_json(serde_json::json!({ "request_id": "Q1", "crew_id": "C1" })).source, "crew_id");
    }

    #[test]
    fn precedence_holds_across_lines_not_just_within_one() {
        // The regression this guards: a coarse key on an early line must not beat a
        // finer key on a later one. Scanning entity-by-entity would get this wrong.
        let c = correlate_lines(&[
            serde_json::json!({ "session_id": "S1" }),
            serde_json::json!({ "run_id": "R1" }),
        ]);
        assert_eq!(c.source, "run_id");
        assert_eq!(c.correlation_key.as_deref(), Some("R1"));
    }

    #[test]
    fn every_known_key_is_recognised() {
        for key in CORRELATION_KEYS {
            let c = correlate_json(serde_json::json!({ *key: "V" }));
            assert_eq!(c.source, *key, "{key} must be recognised");
            assert!(c.is_real_boundary());
        }
    }

    // ── Fallback ─────────────────────────────────────────────────────────────

    #[test]
    fn no_key_falls_back_to_the_sample() {
        let c = correlate("{}", true, "hash-a");
        assert_eq!(c.source, SAMPLE_FALLBACK);
        assert_eq!(c.correlation_key, None);
        assert_eq!(c.task_id, ids::derive_task_id("sample", "hash-a"));
        assert!(!c.is_real_boundary());
    }

    #[test]
    fn no_entities_at_all_still_yields_a_task() {
        // nginx-style samples produce zero entities; they must not panic or produce
        // an empty task id.
        let c = correlate("", true, "hash-empty");
        assert_eq!(c.source, SAMPLE_FALLBACK);
        assert_eq!(c.task_id.len(), 32);
    }

    #[test]
    fn fallback_keeps_samples_apart() {
        assert_ne!(
            correlate("{}", true, "hash-a").task_id,
            correlate("{}", true, "hash-b").task_id,
        );
    }

    // ── Value handling ───────────────────────────────────────────────────────

    #[test]
    fn the_same_key_groups_samples_together() {
        // The whole point of the module: two different samples sharing a session
        // must land in one task.
        let a = correlate(&json_line(serde_json::json!({ "session_id": "S1" })), true, "hash-a");
        let b = correlate(&json_line(serde_json::json!({ "session_id": "S1" })), true, "hash-b");
        assert_eq!(a.task_id, b.task_id, "shared key must merge the samples");
    }

    #[test]
    fn different_values_stay_separate() {
        let a = correlate_json(serde_json::json!({ "session_id": "S1" }));
        let b = correlate_json(serde_json::json!({ "session_id": "S2" }));
        assert_ne!(a.task_id, b.task_id);
    }

    #[test]
    fn numeric_values_are_accepted() {
        // JSON logs sometimes use a numeric run counter.
        let c = correlate_json(serde_json::json!({ "run_id": 42 }));
        assert_eq!(c.source, "run_id");
        assert_eq!(c.correlation_key.as_deref(), Some("42"));
    }

    #[test]
    fn logfmt_string_values_are_accepted() {
        // Logfmt types everything as a string, including numeric ids.
        let c = correlate_json(serde_json::json!({ "run_id": "42" }));
        assert_eq!(c.correlation_key.as_deref(), Some("42"));
    }

    #[test]
    fn blank_and_placeholder_values_fall_back() {
        // An empty key would merge every sample sharing the emptiness into one
        // enormous task — far worse than falling back to sample scope.
        for bad in ["", "  ", "null", "NULL", "none", "nil", "<nil>", "-", "n/a", "unknown", "0"] {
            let c = correlate_json(serde_json::json!({ "session_id": bad }));
            assert_eq!(
                c.source, SAMPLE_FALLBACK,
                "session_id={bad:?} must not become a task boundary",
            );
        }
    }

    #[test]
    fn a_lower_precedence_key_wins_when_the_higher_one_is_blank() {
        let c = correlate_json(serde_json::json!({ "run_id": "", "session_id": "S1" }));
        assert_eq!(c.source, "session_id");
    }

    #[test]
    fn non_scalar_values_are_ignored() {
        let c = correlate_json(serde_json::json!({ "session_id": { "nested": "S1" } }));
        assert_eq!(c.source, SAMPLE_FALLBACK);
    }

    // ── apply ────────────────────────────────────────────────────────────────

    #[test]
    fn apply_stamps_every_entity() {
        let mut entities = vec![plain(), plain(), plain()];
        let c = correlate("{}", true, "hash-a");
        apply(&mut entities, &c);
        assert!(entities.iter().all(|e| e.task_id == c.task_id));
        assert!(entities.iter().all(|e| e.correlation_key.is_none()));
    }

    #[test]
    fn apply_carries_the_correlation_key_through() {
        let mut entities = vec![plain()];
        let c = correlate_json(serde_json::json!({ "session_id": "S1" }));
        apply(&mut entities, &c);
        assert_eq!(entities[0].correlation_key.as_deref(), Some("S1"));
        assert_eq!(entities[0].task_id, c.task_id);
    }

    #[test]
    fn apply_on_an_empty_slice_is_a_no_op() {
        let c = correlate("", true, "h");
        let mut entities: Vec<EntityRecord> = Vec::new();
        apply(&mut entities, &c);
        assert!(entities.is_empty());
    }
}
