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

use std::collections::{BTreeSet, HashMap};

use serde_json::Value;

use super::{entity_extractor, ids};
use crate::models::EntityRecord;

/// Field names that identify a task, most specific first.
///
/// This order is only a **tie-break**, not the primary rule — see [`correlate`]
/// for why coverage wins over specificity. The reasoning behind the order:
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
    /// Keys the sample carried **more than one value of**, and which were
    /// therefore rejected as sample-wide boundaries.
    ///
    /// A non-empty list is the honest signal that this sample contains several
    /// units of work and the chosen boundary is coarser than the log could
    /// actually support. `crewai_logfmt.log` is the worked example: it carries
    /// `task_id=task-1` and `task_id=task-2`, so `task_id` lands here and the
    /// boundary falls through to a key the whole sample agrees on.
    pub spanning_keys: Vec<String>,
}

impl TaskCorrelation {
    /// Whether this task boundary came from the log rather than from the sampler.
    pub fn is_real_boundary(&self) -> bool {
        self.source != SAMPLE_FALLBACK
    }
}

/// How much of a sample one correlation key accounts for.
struct KeyEvidence {
    /// Distinct values seen. Ordered, so selection is deterministic.
    values: BTreeSet<String>,
    /// Lines carrying a usable value — the key's coverage of the sample.
    lines: usize,
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
/// # Selection: coverage first, specificity only as a tie-break
///
/// An earlier version applied [`CORRELATION_KEYS`] precedence globally — the
/// highest-ranked key found *anywhere* won. That is wrong in two ways, both
/// demonstrable on the bundled fixtures:
///
/// **A key can appear once and hijack the sample.** `langchain_json.log` carries
/// `session_id` on two lines and `run_id` on exactly one — line 18, the last.
/// `run_id` outranks `session_id`, so all 18 lines correlated to a value that
/// appeared incidentally at the very end. The damage is not the label but the
/// *instability*: a sample cut at line 17 correlates to the session, cut at line
/// 18 to the run, and two overlapping samples of one agent run land in two
/// disjoint tasks that can never be joined. The task id depended on where the
/// sampler happened to cut, which is exactly the artifact this module exists to
/// remove.
///
/// **A key can hold several values.** `crewai_logfmt.log` carries
/// `task_id=task-1` and `task_id=task-2`. Taking the first stamped `task-1` onto
/// the writer's and reviewer's work too, and reported `is_real_boundary() ==
/// true` while doing it — a confident, specific, wrong attribution, which is
/// worse than the fallback because the fallback is at least labelled as a guess.
///
/// So: a key qualifies only if the whole sample agrees on **one** value for it
/// (multi-valued keys are recorded in [`TaskCorrelation::spanning_keys`] and
/// skipped), and among qualifying keys the one covering the most lines wins,
/// with [`CORRELATION_KEYS`] order breaking ties. Coverage is stable under
/// re-sampling in a way that first-hit is not.
///
/// Falls back to `("sample", sample_hash)` when no key qualifies.
pub fn correlate(
    content: &str,
    is_json: bool,
    sample_hash: &str,
    target_id: &str,
) -> TaskCorrelation {
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

    let mut spanning_keys = Vec::new();
    // (coverage, precedence_rank, key, value) for every key the sample agrees on.
    let mut qualifying: Vec<(usize, usize, &str, String)> = Vec::new();

    for (rank, key) in CORRELATION_KEYS.iter().enumerate() {
        let evidence = gather(&per_line, key);
        match evidence.values.len() {
            0 => {}
            1 => {
                let value = evidence.values.into_iter().next().expect("len checked");
                qualifying.push((evidence.lines, rank, key, value));
            }
            // The sample spans several of this key's values, so it is not a
            // boundary for the sample as a whole. Fall through to a coarser key
            // the sample does agree on rather than picking one arbitrarily.
            _ => spanning_keys.push((*key).to_string()),
        }
    }

    // Most coverage wins; ties go to the more specific key (lowest rank).
    let chosen = qualifying
        .into_iter()
        .max_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));

    match chosen {
        Some((_, _, key, value)) => {
            let scope = (!is_globally_unique(&value)).then_some(target_id);
            TaskCorrelation {
                task_id: ids::derive_task_id(key, &value, scope),
                source: key.to_string(),
                correlation_key: Some(value),
                spanning_keys,
            }
        }
        None => TaskCorrelation {
            // The sample fallback is inherently sample-scoped, and `sample_hash`
            // is already globally unique, so it needs no target scope.
            task_id: ids::derive_task_id(SAMPLE_FALLBACK, sample_hash, None),
            source: SAMPLE_FALLBACK.to_string(),
            correlation_key: None,
            spanning_keys,
        },
    }
}

/// Collect every usable value of one key across the sample.
fn gather(per_line: &[HashMap<String, Value>], key: &str) -> KeyEvidence {
    let mut values = BTreeSet::new();
    let mut lines = 0;
    for fields in per_line {
        if let Some(value) = extract_key(fields, key) {
            values.insert(value);
            lines += 1;
        }
    }
    KeyEvidence { values, lines }
}

/// Whether a correlation value is safe to use as a **cross-source** task key.
///
/// Task ids are deliberately not scoped to a sample, so that a task spanning
/// several samples reassembles. The same property makes a low-entropy value
/// dangerous: `run_id=1` is emitted by every system that numbers runs from one,
/// and hashing it unscoped merges all of them into a single task.
///
/// The test is deliberately blunt — an id long enough to be an id, and not a
/// bare counter. UUIDs, `sess-abc123` and hex digests pass; `1`, `42`, `task-1`
/// and `default` do not, and get scoped to their target instead of rejected, so
/// grouping still works where it is meaningful.
pub fn is_globally_unique(value: &str) -> bool {
    const MIN_GLOBAL_CHARS: usize = 8;
    value.chars().count() >= MIN_GLOBAL_CHARS && !value.chars().all(|c| c.is_ascii_digit())
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

    let lowered = raw.to_ascii_lowercase();
    if raw.is_empty()
        || is_placeholder(&lowered)
        || CORRELATION_PLACEHOLDERS.contains(&lowered.as_str())
    {
        return None;
    }
    Some(raw)
}

/// Values that mean "this field is absent" despite the field being present.
///
/// Shared with [`super::actor_extractor`], because a `<nil>` tool name is as
/// meaningless as a `<nil>` session id and the two lists drifting apart was a
/// real defect: `undefined` — what JavaScript emits for a missing field, and so
/// the single most likely placeholder in practice — was absent from this one,
/// letting `session_id=undefined` become a *real* boundary that merged every
/// service that ever emitted one.
pub(crate) const PLACEHOLDER_VALUES: &[&str] = &[
    "null", "(null)", "nil", "<nil>", "none", "undefined", "unknown", "nan", "-", "--", "n/a",
    "na", "<none>", "<empty>",
];

/// Placeholders that apply to **correlation keys only**.
///
/// A shared constant used as a correlation value merges unrelated work, so these
/// are rejected outright. They are not in [`PLACEHOLDER_VALUES`] because they are
/// legitimate *names*: a tool genuinely called `test` should still get an actor
/// node.
///
/// Note `"0"` is deliberately **not** here. Rejecting it made a zero-indexed
/// `run_id=0` fall back to sample scope while runs 1..n correlated normally —
/// inconsistent grouping inside a single workload. The real concern it was
/// standing in for is low entropy, which [`is_globally_unique`] now handles
/// properly by scoping such values to their target.
const CORRELATION_PLACEHOLDERS: &[&str] = &["default", "test", "example", "todo", "changeme"];

/// Whether a lowercased value is one of the shared placeholders.
pub(crate) fn is_placeholder(lowered: &str) -> bool {
    PLACEHOLDER_VALUES.contains(&lowered)
}

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

    /// The target every helper correlates against, when the test does not care.
    const T: &str = "target-1";

    /// Correlate a single JSON line.
    fn correlate_json(fields: serde_json::Value) -> TaskCorrelation {
        correlate(&json_line(fields), true, "h", T)
    }

    /// Correlate several JSON lines, in order.
    fn correlate_lines(lines: &[serde_json::Value]) -> TaskCorrelation {
        let content = lines.iter().map(|l| l.to_string()).collect::<Vec<_>>().join("\n");
        correlate(&content, true, "h", T)
    }

    /// Correlate `n` copies of a line — the way to give a key coverage.
    fn repeat(line: serde_json::Value, n: usize) -> Vec<serde_json::Value> {
        std::iter::repeat_n(line, n).collect()
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
    fn precedence_breaks_ties_across_lines() {
        // Both keys cover one line each, so specificity decides. The property that
        // matters here is that the *whole sample* is scanned: an earlier version
        // read only entity fields and missed keys on non-entity lines entirely.
        let c = correlate_lines(&[
            serde_json::json!({ "session_id": "S1" }),
            serde_json::json!({ "run_id": "R1" }),
        ]);
        assert_eq!(c.source, "run_id");
        assert_eq!(c.correlation_key.as_deref(), Some("R1"));
    }

    // ── Coverage beats specificity (C2) ──────────────────────────────────────

    #[test]
    fn a_key_appearing_once_does_not_outrank_one_covering_the_sample() {
        // Reproduces `langchain_json.log`: session_id throughout, run_id on a
        // single late line. Precedence alone handed the whole sample to run_id.
        let mut lines = repeat(serde_json::json!({ "session_id": "sess-abc123" }), 5);
        lines.push(serde_json::json!({ "run_id": "run-001" }));

        let c = correlate_lines(&lines);
        assert_eq!(
            c.source, "session_id",
            "the key covering 5 lines must beat the one covering 1",
        );
    }

    #[test]
    fn the_task_id_survives_a_different_sampling_cut() {
        // The real damage of the hijack was instability, not mislabelling: the
        // task id changed with where the sampler happened to cut, so two
        // overlapping samples of one run could never be joined.
        let full = repeat(serde_json::json!({ "session_id": "sess-abc123" }), 5);
        let mut with_tail = full.clone();
        with_tail.push(serde_json::json!({ "run_id": "run-001" }));

        assert_eq!(
            correlate_lines(&full).task_id,
            correlate_lines(&with_tail).task_id,
            "including one more line must not re-key the task",
        );
    }

    // ── A sample can span several tasks (C1) ─────────────────────────────────

    #[test]
    fn a_multi_valued_key_is_not_a_sample_wide_boundary() {
        // Reproduces `crewai_logfmt.log`: task_id=task-1 and task_id=task-2 in one
        // sample. Taking the first stamped task-1 onto the second task's work and
        // reported full confidence in it.
        let c = correlate_lines(&[
            serde_json::json!({ "task_id": "task-1", "crew_id": "crew-42" }),
            serde_json::json!({ "task_id": "task-2", "crew_id": "crew-42" }),
        ]);

        assert_ne!(c.source, "task_id", "task_id disagrees with itself here");
        assert_eq!(c.spanning_keys, vec!["task_id".to_string()]);
        assert_eq!(
            c.source, "crew_id",
            "falls through to a key the whole sample agrees on",
        );
    }

    #[test]
    fn a_sample_spanning_several_tasks_with_no_coarser_key_falls_back() {
        // Nothing to fall through to, so it must say so rather than pick one.
        let c = correlate_lines(&[
            serde_json::json!({ "task_id": "task-1" }),
            serde_json::json!({ "task_id": "task-2" }),
        ]);
        assert_eq!(c.source, SAMPLE_FALLBACK);
        assert!(!c.is_real_boundary());
        assert_eq!(c.spanning_keys, vec!["task_id".to_string()]);
    }

    #[test]
    fn a_single_valued_key_repeated_is_not_spanning() {
        let c = correlate_lines(&repeat(serde_json::json!({ "task_id": "task-1" }), 4));
        assert_eq!(c.source, "task_id");
        assert!(c.spanning_keys.is_empty());
    }

    // ── Low-entropy values are scoped to their target (I1) ───────────────────

    #[test]
    fn a_bare_counter_does_not_merge_across_targets() {
        // `run_id=1` is emitted by every system that numbers runs from one.
        // Unscoped, all of them hashed to a single task.
        let line = json_line(serde_json::json!({ "run_id": 1 }));
        let a = correlate(&line, true, "h", "target-a");
        let b = correlate(&line, true, "h", "target-b");
        assert_ne!(a.task_id, b.task_id);
        // Still a real boundary — scoped, not rejected.
        assert!(a.is_real_boundary());
    }

    #[test]
    fn a_bare_counter_still_groups_within_one_target() {
        let line = json_line(serde_json::json!({ "run_id": 1 }));
        assert_eq!(
            correlate(&line, true, "hash-a", "target-a").task_id,
            correlate(&line, true, "hash-b", "target-a").task_id,
        );
    }

    #[test]
    fn a_high_entropy_value_still_merges_across_targets() {
        // The cross-source case the module was built for must keep working.
        let line = json_line(serde_json::json!({ "session_id": "sess-abc123def" }));
        assert_eq!(
            correlate(&line, true, "h", "target-a").task_id,
            correlate(&line, true, "h", "target-b").task_id,
        );
    }

    #[test]
    fn is_globally_unique_draws_the_line_where_documented() {
        for global in ["sess-abc123", "8f3c1e7a-1111-2222", "0123456789abcdef"] {
            assert!(is_globally_unique(global), "{global:?} should be global");
        }
        for scoped in ["1", "42", "0", "task-1", "run-1", "abc", "1234567890"] {
            assert!(!is_globally_unique(scoped), "{scoped:?} should be scoped");
        }
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
        let c = correlate("{}", true, "hash-a", T);
        assert_eq!(c.source, SAMPLE_FALLBACK);
        assert_eq!(c.correlation_key, None);
        assert_eq!(c.task_id, ids::derive_task_id("sample", "hash-a", None));
        assert!(!c.is_real_boundary());
    }

    #[test]
    fn no_entities_at_all_still_yields_a_task() {
        // nginx-style samples produce zero entities; they must not panic or produce
        // an empty task id.
        let c = correlate("", true, "hash-empty", T);
        assert_eq!(c.source, SAMPLE_FALLBACK);
        assert_eq!(c.task_id.len(), 32);
    }

    #[test]
    fn fallback_keeps_samples_apart() {
        assert_ne!(
            correlate("{}", true, "hash-a", T).task_id,
            correlate("{}", true, "hash-b", T).task_id,
        );
    }

    // ── Value handling ───────────────────────────────────────────────────────

    #[test]
    fn the_same_key_groups_samples_together() {
        // The whole point of the module: two different samples sharing a session
        // must land in one task.
        let a = correlate(&json_line(serde_json::json!({ "session_id": "S1" })), true, "hash-a", T);
        let b = correlate(&json_line(serde_json::json!({ "session_id": "S1" })), true, "hash-b", T);
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
    fn zero_is_a_legitimate_counter_value() {
        // `"0"` used to be treated as a placeholder, which made a zero-indexed
        // run fall back to sample scope while runs 1..n correlated normally —
        // inconsistent grouping inside a single workload.
        let c = correlate_json(serde_json::json!({ "run_id": 0 }));
        assert_eq!(c.source, "run_id");
        assert_eq!(c.correlation_key.as_deref(), Some("0"));
        // It is low-entropy, so it is scoped rather than global — see
        // `a_bare_counter_does_not_merge_across_targets`.
        assert!(!is_globally_unique("0"));
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
        for bad in [
            "", "  ", "null", "NULL", "(null)", "none", "nil", "<nil>", "-", "n/a", "nan",
            "unknown",
            // The one that mattered most in practice and was missing: JavaScript
            // stringifies a missing field to this, so `session_id=undefined` was
            // becoming a real boundary that merged every service emitting one.
            "undefined", "UNDEFINED",
            // Shared constants merge unrelated work just as effectively as blanks.
            "default", "test",
        ] {
            let c = correlate_json(serde_json::json!({ "session_id": bad }));
            assert_eq!(
                c.source, SAMPLE_FALLBACK,
                "session_id={bad:?} must not become a task boundary",
            );
        }
    }

    #[test]
    fn actor_names_and_correlation_keys_share_one_placeholder_list() {
        // The two lists had drifted, each missing entries the other had. This
        // pins the shared core so they cannot drift again silently.
        for value in ["null", "nil", "<nil>", "none", "undefined", "unknown", "n/a"] {
            assert!(is_placeholder(value), "{value:?} must be a placeholder");
        }
        // ...but a tool genuinely called `test` is still a real skill, so the
        // correlation-only extras must stay out of the shared list.
        assert!(!is_placeholder("test"));
        assert!(!is_placeholder("default"));
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
        let c = correlate("{}", true, "hash-a", T);
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
        let c = correlate("", true, "h", T);
        let mut entities: Vec<EntityRecord> = Vec::new();
        apply(&mut entities, &c);
        assert!(entities.is_empty());
    }
}
