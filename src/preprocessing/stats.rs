//! Computes quantitative statistics from the raw sample text.
//!
//! All numeric output fields use `i64` or `f64` to remain BSON-safe.  The
//! level distribution extracts severity labels from JSON, Logfmt, and Syslog
//! lines so that downstream consumers can assess sample composition without
//! re-parsing the raw text.

use std::collections::{HashMap, HashSet};

use crate::models::{LogType, SampleStats};

/// Maximum number of lines read when building statistics.
const MAX_STAT_LINES: usize = 500;

/// Known level keyword variants mapped to a canonical label.
const LEVEL_ALIASES: &[(&str, &str)] = &[
    ("debug", "debug"),
    ("dbg", "debug"),
    ("trace", "trace"),
    ("trc", "trace"),
    ("info", "info"),
    ("information", "info"),
    ("inf", "info"),
    ("notice", "notice"),
    ("warn", "warn"),
    ("warning", "warn"),
    ("wrn", "warn"),
    ("error", "error"),
    ("err", "error"),
    ("critical", "critical"),
    ("crit", "critical"),
    ("fatal", "fatal"),
    ("alert", "alert"),
    ("emergency", "emergency"),
    ("emerg", "emergency"),
];

/// Compute statistics for `content` whose detected format type is `log_type`.
pub fn compute(content: &str, log_type: &LogType) -> SampleStats {
    let lines: Vec<&str> = content.lines().take(MAX_STAT_LINES).collect();
    let total = lines.len() as i64;

    let non_empty: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let non_empty_count = non_empty.len() as i64;

    let empty_line_ratio = if total == 0 {
        0.0
    } else {
        (total - non_empty_count) as f64 / total as f64
    };

    let avg_line_length = if non_empty_count == 0 {
        0.0
    } else {
        non_empty.iter().map(|l| l.len()).sum::<usize>() as f64 / non_empty_count as f64
    };

    let unique_count = non_empty.iter().collect::<HashSet<_>>().len() as i64;
    let unique_line_ratio = if non_empty_count == 0 {
        0.0
    } else {
        unique_count as f64 / non_empty_count as f64
    };

    let level_distribution = extract_level_distribution(&non_empty, log_type);

    SampleStats {
        total_lines: total,
        non_empty_lines: non_empty_count,
        empty_line_ratio,
        avg_line_length,
        time_span_secs: None, // Timestamp parsing deferred to a future phase.
        level_distribution,
        unique_line_ratio,
    }
}

// ─── Level distribution ───────────────────────────────────────────────────────

fn extract_level_distribution(
    lines: &[&str],
    log_type: &LogType,
) -> HashMap<String, i64> {
    let mut counts: HashMap<String, i64> = HashMap::new();

    for &line in lines {
        let label = match log_type {
            LogType::Json => extract_level_json(line),
            LogType::Logfmt => extract_level_logfmt(line),
            LogType::Syslog => extract_level_syslog(line),
            _ => extract_level_plain(line),
        };

        if let Some(canonical) = label {
            *counts.entry(canonical).or_insert(0) += 1;
        }
    }

    counts
}

fn normalise_level(raw: &str) -> Option<String> {
    let lower = raw.trim().trim_matches('"').to_ascii_lowercase();
    LEVEL_ALIASES
        .iter()
        .find(|(alias, _)| *alias == lower.as_str())
        .map(|(_, canonical)| (*canonical).to_string())
}

fn extract_level_json(line: &str) -> Option<String> {
    // Quick scan for `"level":"..."` or `"severity":"..."`.
    for key in ["\"level\"", "\"severity\"", "\"lvl\"", "\"log_level\""] {
        if let Some(pos) = line.find(key) {
            let after = &line[pos + key.len()..].trim_start();
            if after.starts_with(':') {
                let value_part = after[1..].trim_start();
                let raw = if value_part.starts_with('"') {
                    value_part[1..].split('"').next().unwrap_or("")
                } else {
                    value_part.split([',', '}', ' ']).next().unwrap_or("")
                };
                if let Some(label) = normalise_level(raw) {
                    return Some(label);
                }
            }
        }
    }
    None
}

fn extract_level_logfmt(line: &str) -> Option<String> {
    for token in line.split_whitespace() {
        if let Some((key, value)) = token.split_once('=') {
            let lower_key = key.to_ascii_lowercase();
            if ["level", "lvl", "severity", "log_level"].contains(&lower_key.as_str()) {
                return normalise_level(value);
            }
        }
    }
    None
}

fn extract_level_syslog(line: &str) -> Option<String> {
    // Syslog severity is encoded in the priority value `<facility*8 + severity>`.
    // We also scan for keyword patterns in the message body for human-readable forms.
    if line.starts_with('<') {
        if let Some(end) = line.find('>') {
            if let Ok(priority) = line[1..end].parse::<u8>() {
                let severity = priority & 0b111;
                let label = match severity {
                    0 => "emergency",
                    1 => "alert",
                    2 => "critical",
                    3 => "error",
                    4 => "warn",
                    5 => "notice",
                    6 => "info",
                    7 => "debug",
                    _ => return None,
                };
                return Some(label.to_string());
            }
        }
    }
    extract_level_plain(line)
}

fn extract_level_plain(line: &str) -> Option<String> {
    // Scan for bracketed or bare level keywords: `[INFO]`, `ERROR:`, `WARN `, etc.
    let upper = line.to_ascii_uppercase();
    for (alias, canonical) in LEVEL_ALIASES {
        let upper_alias = alias.to_ascii_uppercase();
        if upper.contains(&format!("[{upper_alias}]"))
            || upper.contains(&format!("{upper_alias}:"))
            || upper.contains(&format!(" {upper_alias} "))
            || upper.starts_with(&upper_alias)
        {
            return Some((*canonical).to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("fixture not found: {}", path.display()))
    }

    #[test]
    fn test_stats_basic_counts() {
        let content = fixture("langchain_json.log");
        let stats = compute(&content, &LogType::Json);

        assert_eq!(stats.total_lines, 20, "expected 20 lines");
        assert_eq!(stats.non_empty_lines, 20);
        assert!((stats.empty_line_ratio - 0.0).abs() < f64::EPSILON);
        assert!(stats.avg_line_length > 50.0, "lines should be reasonably long");
    }

    #[test]
    fn test_stats_level_distribution_json() {
        let content = fixture("langchain_json.log");
        let stats = compute(&content, &LogType::Json);

        assert!(stats.level_distribution.contains_key("info"), "should detect info");
        assert!(stats.level_distribution.contains_key("debug"), "should detect debug");

        let info_count = stats.level_distribution["info"];
        let debug_count = stats.level_distribution["debug"];
        assert!(info_count > 0);
        assert!(debug_count > 0);
        assert!(info_count > debug_count, "more info than debug in fixture");
    }

    #[test]
    fn test_stats_level_distribution_logfmt() {
        let content = fixture("crewai_logfmt.log");
        let stats = compute(&content, &LogType::Logfmt);

        assert!(stats.level_distribution.contains_key("info"));
        assert!(stats.level_distribution.contains_key("debug"));
    }

    #[test]
    fn test_stats_unique_line_ratio() {
        let content = fixture("langchain_json.log");
        let stats = compute(&content, &LogType::Json);
        // All fixture lines are unique
        assert!(
            stats.unique_line_ratio > 0.9,
            "expected high uniqueness, got {}",
            stats.unique_line_ratio
        );
    }

    #[test]
    fn test_stats_repetitive_content() {
        // Duplicate the same line many times
        let line = r#"{"level":"info","msg":"heartbeat"}"#;
        let content = std::iter::repeat(line).take(20).collect::<Vec<_>>().join("\n");
        let stats = compute(&content, &LogType::Json);

        assert!(
            stats.unique_line_ratio < 0.1,
            "repetitive content should have low unique ratio, got {}",
            stats.unique_line_ratio
        );
    }

    #[test]
    fn test_stats_empty_content() {
        let stats = compute("", &LogType::PlainText);
        assert_eq!(stats.total_lines, 0);
        assert_eq!(stats.non_empty_lines, 0);
        assert!((stats.empty_line_ratio - 0.0).abs() < f64::EPSILON);
        assert!((stats.avg_line_length - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_stats_plain_text_level_detection() {
        let content = fixture("bedrock_multiline.log");
        let stats = compute(&content, &LogType::PlainText);

        assert!(
            stats.level_distribution.contains_key("info")
                || stats.level_distribution.contains_key("error")
                || stats.level_distribution.contains_key("warn"),
            "should detect at least one level in bedrock log, got: {:?}",
            stats.level_distribution
        );
    }
}
