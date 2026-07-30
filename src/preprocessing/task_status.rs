//! Stage 14 — derive a **task status** from log evidence.
//!
//! The audit use case starts with "show me the active tasks", which needs a task
//! to have a state. Nothing in the pipeline had one: [`crate::models::TaskRecord`]
//! carried `first_seen` and `last_seen` but no notion of whether the work was
//! still running, had finished, or had failed.
//!
//! # This is inference, not fact
//!
//! A log is evidence about a task, not a record of it. A sample is a window onto
//! a file that was probably still being written. So every value here is a
//! reading of evidence and can be wrong in both directions, and the module is
//! built to be wrong in the *safe* direction:
//!
//! * An error is strong evidence and wins outright.
//! * A terminal marker near the end of the sample is decent evidence of
//!   completion.
//! * **Absence of a completion signal means [`TaskStatus::Running`], not
//!   completed.** We did not see it end, so we must not claim it ended. That is
//!   also what makes "active tasks" a useful list rather than a list of
//!   everything.
//!
//! # Why the vocabulary is VectaDB's
//!
//! `running` / `completed` / `failed` are exactly the values `Run.status` and
//! `Task.status` admit in `vectadb_trace.json`, whose enum constraint is
//! hard-enforced. Inventing a fourth value here — `unknown`, say, which is
//! arguably more honest — would make every task a 400 on export. The honesty is
//! carried by `Running` meaning "no evidence it ended" rather than by a new
//! variant.

use serde::{Deserialize, Serialize};

use super::otel_builder::{OtelSpan, StatusCode};

/// A task's state, as far as the log shows.
///
/// Ordered by strength of claim: see [`TaskStatus::rank`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// No evidence the work ended. The default, deliberately.
    #[default]
    Running,
    /// A terminal marker was seen and nothing errored.
    Completed,
    /// Something errored. Outranks everything else.
    Failed,
}

impl TaskStatus {
    /// The wire value, matching the `Task.status` enum in the VectaDB ontology.
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskStatus::Running => "running",
            TaskStatus::Completed => "completed",
            TaskStatus::Failed => "failed",
        }
    }

    /// Monotonic strength, so a task's status can only ever move upward.
    ///
    /// A task spans samples, and they are not processed in log order. Storing the
    /// rank and combining with `$max` means the recorded status does not depend
    /// on arrival order, and — the point for an audit — a failure once observed is
    /// never overwritten by a later sample that happened to look clean.
    pub fn rank(&self) -> u8 {
        match self {
            TaskStatus::Running => 0,
            TaskStatus::Completed => 1,
            TaskStatus::Failed => 2,
        }
    }

    /// Inverse of [`TaskStatus::rank`], for reading a stored value back.
    pub fn from_rank(rank: u8) -> Self {
        match rank {
            2 => TaskStatus::Failed,
            1 => TaskStatus::Completed,
            _ => TaskStatus::Running,
        }
    }

    /// Inverse of [`TaskStatus::as_str`], for validating a caller-supplied
    /// `?status=` filter against the same three wire values.
    ///
    /// Returns `None` rather than defaulting to [`TaskStatus::Running`] on a bad
    /// value — a typo in a filter must surface as a 400, not silently narrow the
    /// query to "running" and hide the rest of the tasks.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "running" => Some(TaskStatus::Running),
            "completed" => Some(TaskStatus::Completed),
            "failed" => Some(TaskStatus::Failed),
            _ => None,
        }
    }
}

/// Phrases that mark a unit of work *ending*.
///
/// Drawn from what the bundled fixtures actually contain rather than invented:
/// `crewai_logfmt.log` has `msg="crew.kickoff finished"` and
/// `msg="Task( crew-42 ) completed successfully"`, `mcp_session.log` has
/// `session end`, and the ReAct and langchain logs end on `Final Answer`.
///
/// Note the overlap with [`super::task_intent`]'s noise list is deliberate and
/// inverted: lifecycle chatter is exactly what must not be mistaken for a goal
/// statement, and exactly what tells you the work stopped.
const TERMINAL_MARKERS: &[&str] = &[
    "session end",
    "session ended",
    "run finished",
    "run complete",
    "kickoff finished",
    "kickoff complete",
    "completed successfully",
    "task complete",
    "task finished",
    "final answer",
    "execution complete",
    "workflow complete",
];

/// Fraction of the sample, measured from the end, in which a terminal marker
/// counts.
///
/// A marker's meaning depends on where it sits. "completed" in the first line is
/// usually someone *asking* about completion — a prompt reading "tell me when the
/// migration is completed" would otherwise mark the task done before it began.
/// Requiring terminal position is what separates the signal from that.
const TAIL_FRACTION: f32 = 0.34;

/// Minimum number of trailing lines to examine, for samples too short for the
/// fraction to mean anything.
const MIN_TAIL_LINES: usize = 3;

/// Derive a task's status from a sample's content and its spans.
///
/// Spans are used for the error signal rather than re-scanning the text, because
/// [`super::otel_builder`] already does that carefully — it reads the
/// conventional error keys, understands that logfmt stringifies everything so
/// `error=false` is the *commonest* spelling of "nothing went wrong", and treats
/// an abnormal `finish_reason` as a failure. Re-implementing that here would
/// reproduce a bug that has already been fixed once.
pub fn derive(content: &str, spans: &[OtelSpan]) -> TaskStatus {
    // 1. An error anywhere in the sample. Strongest evidence, so it wins.
    if spans.iter().any(|s| s.status.code == StatusCode::Error) {
        return TaskStatus::Failed;
    }

    // 2. A terminal marker in the tail.
    if has_terminal_marker(content) {
        return TaskStatus::Completed;
    }

    // 3. No evidence it ended.
    TaskStatus::Running
}

/// Whether the tail of the sample contains a terminal lifecycle marker.
fn has_terminal_marker(content: &str) -> bool {
    let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return false;
    }

    let tail_len = ((lines.len() as f32 * TAIL_FRACTION).ceil() as usize)
        .max(MIN_TAIL_LINES)
        .min(lines.len());
    let tail = &lines[lines.len() - tail_len..];

    tail.iter().any(|line| {
        let lowered = line.to_ascii_lowercase();
        TERMINAL_MARKERS.contains_marker(&lowered)
    })
}

/// Small helper so the marker scan reads as one thought at the call site.
trait ContainsMarker {
    fn contains_marker(&self, haystack: &str) -> bool;
}

impl ContainsMarker for &[&str] {
    fn contains_marker(&self, haystack: &str) -> bool {
        self.iter().any(|m| haystack.contains(m))
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preprocessing::otel_builder::{SpanKind, SpanStatus};
    use std::collections::HashMap;

    fn span(code: StatusCode) -> OtelSpan {
        OtelSpan {
            trace_id: "t".to_string(),
            span_id: "s".to_string(),
            parent_span_id: None,
            name: "n".to_string(),
            kind: SpanKind::Internal,
            start_time_unix_nano: 0,
            end_time_unix_nano: 0,
            attributes: HashMap::new(),
            status: SpanStatus {
                code,
                message: String::new(),
            },
            sample_hash: "h".to_string(),
        }
    }

    // ── The vocabulary must match the ontology ───────────────────────────────

    #[test]
    fn the_wire_values_are_the_ones_the_ontology_admits() {
        // Task.status carries a hard-enforced Enum constraint of exactly these
        // three. A fourth value, or a different spelling, is a 400 on every task.
        assert_eq!(TaskStatus::Running.as_str(), "running");
        assert_eq!(TaskStatus::Completed.as_str(), "completed");
        assert_eq!(TaskStatus::Failed.as_str(), "failed");
    }

    #[test]
    fn rank_round_trips_and_orders_by_strength() {
        for s in [TaskStatus::Running, TaskStatus::Completed, TaskStatus::Failed] {
            assert_eq!(TaskStatus::from_rank(s.rank()), s);
        }
        assert!(TaskStatus::Failed.rank() > TaskStatus::Completed.rank());
        assert!(TaskStatus::Completed.rank() > TaskStatus::Running.rank());
    }

    #[test]
    fn an_unknown_rank_reads_as_running_not_as_a_panic() {
        // Forward compatibility: a rank written by a newer version must degrade
        // to the safe value rather than crash a reader.
        assert_eq!(TaskStatus::from_rank(9), TaskStatus::Running);
    }

    #[test]
    fn the_default_is_running() {
        // Because `#[serde(default)]` on a stored document, and an absent value,
        // must both mean "we do not know that it ended".
        assert_eq!(TaskStatus::default(), TaskStatus::Running);
    }

    #[test]
    fn parse_round_trips_with_as_str() {
        for s in [TaskStatus::Running, TaskStatus::Completed, TaskStatus::Failed] {
            assert_eq!(TaskStatus::parse(s.as_str()), Some(s));
        }
    }

    #[test]
    fn parse_rejects_anything_not_in_the_ontology() {
        // A typo in `?status=` must 400, not silently fall through to a default
        // that quietly returns the wrong set of tasks.
        assert_eq!(TaskStatus::parse("unknown"), None);
        assert_eq!(TaskStatus::parse("Running"), None);
        assert_eq!(TaskStatus::parse(""), None);
    }

    // ── Precedence ───────────────────────────────────────────────────────────

    #[test]
    fn an_error_outranks_a_terminal_marker() {
        // `mcp_session.log` is the real case: it contains an error *and*
        // `session end`. A task that errored and then shut down cleanly has
        // still failed, and an audit must see that.
        let content = "line one\nline two\nmsg=\"session end\"";
        assert_eq!(
            derive(content, &[span(StatusCode::Error)]),
            TaskStatus::Failed,
        );
    }

    #[test]
    fn a_terminal_marker_with_no_error_completes() {
        let content = "starting\nworking\nmsg=\"crew.kickoff finished\"";
        assert_eq!(derive(content, &[span(StatusCode::Ok)]), TaskStatus::Completed);
    }

    #[test]
    fn no_evidence_means_running_not_completed() {
        // The load-bearing default. Claiming completion from silence would make
        // the active-task list empty and the audit trail wrong.
        let content = "just\nsome\nordinary\nlines";
        assert_eq!(derive(content, &[span(StatusCode::Ok)]), TaskStatus::Running);
        assert_eq!(derive(content, &[]), TaskStatus::Running);
    }

    #[test]
    fn an_unset_span_status_is_not_an_error() {
        // Most lines carry no verdict either way; only an affirmative Error
        // counts, or every task would read as failed.
        assert_eq!(
            derive("nothing here", &[span(StatusCode::Unset)]),
            TaskStatus::Running,
        );
    }

    // ── Position matters ─────────────────────────────────────────────────────

    #[test]
    fn a_marker_in_a_leading_prompt_does_not_complete_the_task() {
        // The false positive worth guarding: a user asking about completion is
        // not a report of completion. This is the "plausible but wrong" shape
        // that has bitten every other stage in this pipeline.
        let mut lines =
            vec!["prompt: tell me when the migration is completed successfully".to_string()];
        for i in 0..20 {
            lines.push(format!("step {i} in progress"));
        }
        let content = lines.join("\n");
        assert_eq!(
            derive(&content, &[span(StatusCode::Ok)]),
            TaskStatus::Running,
            "an early mention must not be read as a terminal marker",
        );
    }

    #[test]
    fn a_marker_in_the_tail_does_complete_the_task() {
        let mut lines: Vec<String> = (0..20).map(|i| format!("step {i}")).collect();
        lines.push("msg=\"run finished\"".to_string());
        assert_eq!(
            derive(&lines.join("\n"), &[span(StatusCode::Ok)]),
            TaskStatus::Completed,
        );
    }

    #[test]
    fn short_samples_still_get_a_usable_tail() {
        // With a 34% fraction a two-line sample would look at one line; the
        // floor keeps the whole thing in scope rather than making the rule
        // depend on sample length.
        assert_eq!(
            derive("working\nmsg=\"session ended\"", &[]),
            TaskStatus::Completed,
        );
        assert_eq!(derive("msg=\"session ended\"", &[]), TaskStatus::Completed);
    }

    #[test]
    fn markers_are_matched_case_insensitively() {
        assert_eq!(derive("a\nb\nRUN FINISHED", &[]), TaskStatus::Completed);
        assert_eq!(derive("a\nb\nFinal Answer: 42", &[]), TaskStatus::Completed);
    }

    #[test]
    fn an_empty_sample_is_running() {
        assert_eq!(derive("", &[]), TaskStatus::Running);
        assert_eq!(derive("\n\n  \n", &[]), TaskStatus::Running);
    }
}
