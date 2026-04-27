//! Produces [`IngestionHints`] from the other preprocessing outputs.
//!
//! The hints module is the final stage of the pipeline.  It synthesises the
//! format, stats, and agentic scan results into actionable guidance for the
//! downstream LLM classifier (logflayersense):
//!
//! - **`prompt_template`** — which prompt variant best matches this sample.
//! - **`suggested_chunk_size`** — how many log lines to include per LLM call.
//! - **`worth_classifying`** — whether to send this sample to the LLM at all.
//! - **`skip_reason`** — human-readable explanation when skipping.
//! - **`priority`** — processing urgency derived from the agentic signal score.

use crate::models::{AgenticScan, IngestionHints, LogFormat, LogType, PromptTemplate, SampleStats};

/// Derive [`IngestionHints`] from the outputs of previous pipeline stages.
pub fn derive(
    format: &LogFormat,
    stats: &SampleStats,
    agentic: &AgenticScan,
) -> IngestionHints {
    // ── Skip heuristics ───────────────────────────────────────────────────────
    if stats.non_empty_lines == 0 {
        return skip("sample has no non-empty lines");
    }

    if stats.unique_line_ratio < 0.05 {
        return skip("sample is highly repetitive (unique_line_ratio < 0.05)");
    }

    if stats.avg_line_length < 10.0 {
        return skip("average line length too short to be informative");
    }

    // ── Template selection ────────────────────────────────────────────────────
    let prompt_template = select_template(format, agentic);

    // ── Chunk size ────────────────────────────────────────────────────────────
    // JSON lines tend to be verbose; give them smaller chunks.
    // Syslog and plain text can tolerate more lines per call.
    let suggested_chunk_size: i32 = match format.log_type {
        LogType::Json => 20,
        LogType::Logfmt => 30,
        LogType::Syslog => 50,
        LogType::Multiline => 10, // multiline events are already large
        LogType::PlainText => 40,
    };

    // ── Priority ─────────────────────────────────────────────────────────────
    // Scale 0–100 from signal_score (0.0–1.0).
    let priority = (agentic.signal_score * 100.0).round().min(100.0) as i32;

    IngestionHints {
        prompt_template,
        suggested_chunk_size,
        worth_classifying: agentic.worth_classifying,
        skip_reason: if !agentic.worth_classifying {
            Some(format!(
                "agentic signal score {:.4} is below threshold",
                agentic.signal_score
            ))
        } else {
            None
        },
        priority,
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn skip(reason: &str) -> IngestionHints {
    IngestionHints {
        prompt_template: PromptTemplate::Generic,
        suggested_chunk_size: 40,
        worth_classifying: false,
        skip_reason: Some(reason.to_string()),
        priority: 0,
    }
}

fn select_template(format: &LogFormat, agentic: &AgenticScan) -> PromptTemplate {
    // Use the agentic-specific templates when the scan found meaningful signals
    // and the format is structured enough to warrant them.
    let has_signal = agentic.worth_classifying || agentic.signal_score > 0.0;

    match (&format.log_type, has_signal) {
        (LogType::Json, true) => PromptTemplate::JsonAgent,
        (LogType::Logfmt, true) => PromptTemplate::LogfmtAgent,
        (LogType::Syslog, _) => PromptTemplate::Syslog,
        (LogType::Json, false) => PromptTemplate::JsonAgent, // still use structured template
        (LogType::Logfmt, false) => PromptTemplate::LogfmtAgent,
        _ => PromptTemplate::Generic,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_format(log_type: LogType) -> LogFormat {
        LogFormat {
            log_type,
            timestamp_field: None,
            level_field: None,
            message_field: None,
            timestamp_format: None,
            multiline: false,
        }
    }

    fn make_stats(non_empty_lines: i64, avg_line_length: f64, unique_ratio: f64) -> SampleStats {
        SampleStats {
            total_lines: non_empty_lines,
            non_empty_lines,
            empty_line_ratio: 0.0,
            avg_line_length,
            time_span_secs: None,
            level_distribution: HashMap::new(),
            unique_line_ratio: unique_ratio,
        }
    }

    fn make_agentic(score: f64, threshold: f64) -> AgenticScan {
        AgenticScan {
            signal_score: score,
            worth_classifying: score >= threshold,
            detected_frameworks: vec![],
            matched_patterns: vec![],
            agentic_line_count: (score * 100.0) as i64,
        }
    }

    #[test]
    fn test_template_json_agent() {
        let format = make_format(LogType::Json);
        let stats = make_stats(20, 120.0, 0.9);
        let agentic = make_agentic(0.5, 0.02);
        let hints = derive(&format, &stats, &agentic);
        assert_eq!(hints.prompt_template, PromptTemplate::JsonAgent);
    }

    #[test]
    fn test_template_logfmt_agent() {
        let format = make_format(LogType::Logfmt);
        let stats = make_stats(20, 80.0, 0.9);
        let agentic = make_agentic(0.4, 0.02);
        let hints = derive(&format, &stats, &agentic);
        assert_eq!(hints.prompt_template, PromptTemplate::LogfmtAgent);
    }

    #[test]
    fn test_template_syslog() {
        let format = make_format(LogType::Syslog);
        let stats = make_stats(20, 100.0, 0.9);
        let agentic = make_agentic(0.0, 0.02);
        let hints = derive(&format, &stats, &agentic);
        assert_eq!(hints.prompt_template, PromptTemplate::Syslog);
    }

    #[test]
    fn test_template_generic_plaintext() {
        let format = make_format(LogType::PlainText);
        let stats = make_stats(20, 100.0, 0.9);
        let agentic = make_agentic(0.0, 0.02);
        let hints = derive(&format, &stats, &agentic);
        assert_eq!(hints.prompt_template, PromptTemplate::Generic);
    }

    #[test]
    fn test_skip_empty_sample() {
        let format = make_format(LogType::Json);
        let stats = make_stats(0, 0.0, 0.0);
        let agentic = make_agentic(0.0, 0.02);
        let hints = derive(&format, &stats, &agentic);
        assert!(!hints.worth_classifying);
        assert!(hints.skip_reason.is_some());
    }

    #[test]
    fn test_skip_highly_repetitive() {
        let format = make_format(LogType::Json);
        let stats = make_stats(100, 50.0, 0.02); // unique_ratio=0.02 < 0.05
        let agentic = make_agentic(0.5, 0.02);
        let hints = derive(&format, &stats, &agentic);
        assert!(!hints.worth_classifying);
        assert!(hints.skip_reason.as_deref().unwrap_or("").contains("repetitive"));
    }

    #[test]
    fn test_chunk_size_by_format() {
        let stats = make_stats(20, 100.0, 0.9);
        let agentic = make_agentic(0.5, 0.02);

        let json_hints = derive(&make_format(LogType::Json), &stats, &agentic);
        let syslog_hints = derive(&make_format(LogType::Syslog), &stats, &agentic);

        assert!(
            json_hints.suggested_chunk_size < syslog_hints.suggested_chunk_size,
            "JSON (verbose) should have smaller chunk size than Syslog"
        );
    }

    #[test]
    fn test_priority_scales_with_signal() {
        let format = make_format(LogType::Json);
        let stats = make_stats(20, 100.0, 0.9);

        let low = derive(&format, &stats, &make_agentic(0.1, 0.02));
        let high = derive(&format, &stats, &make_agentic(0.9, 0.02));

        assert!(high.priority > low.priority, "higher signal should yield higher priority");
    }

    #[test]
    fn test_worth_classifying_propagated() {
        let format = make_format(LogType::Json);
        let stats = make_stats(20, 100.0, 0.9);
        let agentic = make_agentic(0.5, 0.02);
        let hints = derive(&format, &stats, &agentic);

        assert!(hints.worth_classifying);
        assert!(hints.skip_reason.is_none());
    }
}
