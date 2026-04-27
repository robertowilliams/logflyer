//! Detects the structural format of a raw log sample.
//!
//! The detector uses a heuristic voting approach: it examines up to
//! `MAX_PROBE_LINES` non-empty lines and scores each candidate format.  The
//! format with the highest score wins.  The algorithm is intentionally simple
//! and allocation-light so it can run inside `spawn_blocking` without
//! measurable latency impact.

use crate::models::{LogFormat, LogType};

/// Maximum number of lines inspected during format detection.
const MAX_PROBE_LINES: usize = 50;

/// Detect the format of `content` and return a populated [`LogFormat`].
pub fn detect(content: &str) -> LogFormat {
    let lines: Vec<&str> = content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(MAX_PROBE_LINES)
        .collect();

    if lines.is_empty() {
        return plain_text_format();
    }

    let json_score = score_json(&lines);
    let logfmt_score = score_logfmt(&lines);
    let syslog_score = score_syslog(&lines);
    let multiline_score = score_multiline(content);

    let best = [
        (LogType::Json, json_score),
        (LogType::Logfmt, logfmt_score),
        (LogType::Syslog, syslog_score),
        (LogType::Multiline, multiline_score),
    ]
    .into_iter()
    .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

    match best {
        Some((log_type, score)) if score > 0.5 => build_format(log_type, &lines),
        _ => plain_text_format(),
    }
}

// ─── Scoring helpers ──────────────────────────────────────────────────────────

fn score_json(lines: &[&str]) -> f64 {
    let hits = lines
        .iter()
        .filter(|line| {
            let trimmed = line.trim();
            trimmed.starts_with('{') && trimmed.ends_with('}')
        })
        .count();
    hits as f64 / lines.len() as f64
}

fn score_logfmt(lines: &[&str]) -> f64 {
    // A logfmt line contains at least two `key=value` pairs with no leading `{`.
    let hits = lines
        .iter()
        .filter(|line| {
            if line.starts_with('{') {
                return false;
            }
            let pair_count = line
                .split_whitespace()
                .filter(|token| token.contains('='))
                .count();
            pair_count >= 2
        })
        .count();
    hits as f64 / lines.len() as f64
}

fn score_syslog(lines: &[&str]) -> f64 {
    // Syslog lines often start with `<priority>` or a RFC-3164 timestamp.
    // We look for the priority prefix and the classic `Mmm DD HH:MM:SS` prefix.
    let hits = lines
        .iter()
        .filter(|line| {
            line.starts_with('<')
                || looks_like_rfc3164_timestamp(line)
                || looks_like_rfc5424_timestamp(line)
        })
        .count();
    hits as f64 / lines.len() as f64
}

fn score_multiline(content: &str) -> f64 {
    // A multiline block is characterised by indented continuation lines.
    let all_lines: Vec<&str> = content.lines().collect();
    if all_lines.len() < 4 {
        return 0.0;
    }
    let indented = all_lines
        .iter()
        .skip(1)
        .filter(|line| line.starts_with("  ") || line.starts_with('\t'))
        .count();
    (indented as f64 / all_lines.len() as f64).min(1.0)
}

// ─── Format builders ──────────────────────────────────────────────────────────

fn build_format(log_type: LogType, lines: &[&str]) -> LogFormat {
    match log_type {
        LogType::Json => build_json_format(lines),
        LogType::Logfmt => build_logfmt_format(lines),
        LogType::Syslog => LogFormat {
            log_type: LogType::Syslog,
            timestamp_field: Some("timestamp".to_string()),
            level_field: Some("severity".to_string()),
            message_field: Some("message".to_string()),
            timestamp_format: None,
            multiline: false,
        },
        LogType::Multiline => LogFormat {
            log_type: LogType::Multiline,
            timestamp_field: None,
            level_field: None,
            message_field: None,
            timestamp_format: None,
            multiline: true,
        },
        LogType::PlainText => plain_text_format(),
    }
}

fn build_json_format(lines: &[&str]) -> LogFormat {
    // Probe the first parseable JSON line for common field names.
    let (ts_field, level_field, msg_field) = lines
        .iter()
        .find_map(|line| parse_json_field_hints(line))
        .unwrap_or((None, None, None));

    LogFormat {
        log_type: LogType::Json,
        timestamp_field: ts_field,
        level_field,
        message_field: msg_field,
        timestamp_format: None,
        multiline: false,
    }
}

fn build_logfmt_format(lines: &[&str]) -> LogFormat {
    let (ts_field, level_field, msg_field) = lines
        .iter()
        .find_map(|line| parse_logfmt_field_hints(line))
        .unwrap_or((None, None, None));

    LogFormat {
        log_type: LogType::Logfmt,
        timestamp_field: ts_field,
        level_field,
        message_field: msg_field,
        timestamp_format: None,
        multiline: false,
    }
}

fn plain_text_format() -> LogFormat {
    LogFormat {
        log_type: LogType::PlainText,
        timestamp_field: None,
        level_field: None,
        message_field: None,
        timestamp_format: None,
        multiline: false,
    }
}

// ─── Field-name heuristics ────────────────────────────────────────────────────

const TIMESTAMP_KEYS: &[&str] = &["time", "timestamp", "ts", "@timestamp", "date", "datetime"];
const LEVEL_KEYS: &[&str] = &["level", "severity", "log_level", "lvl", "loglevel"];
const MESSAGE_KEYS: &[&str] = &["message", "msg", "text", "body", "log"];

fn parse_json_field_hints(
    line: &str,
) -> Option<(Option<String>, Option<String>, Option<String>)> {
    // Minimal JSON key scanner — avoids pulling in a full parser.
    let mut ts = None;
    let mut level = None;
    let mut msg = None;

    for key in extract_json_keys(line) {
        let lower = key.to_ascii_lowercase();
        if ts.is_none() && TIMESTAMP_KEYS.contains(&lower.as_str()) {
            ts = Some(key.to_string());
        } else if level.is_none() && LEVEL_KEYS.contains(&lower.as_str()) {
            level = Some(key.to_string());
        } else if msg.is_none() && MESSAGE_KEYS.contains(&lower.as_str()) {
            msg = Some(key.to_string());
        }
    }

    if ts.is_some() || level.is_some() || msg.is_some() {
        Some((ts, level, msg))
    } else {
        None
    }
}

fn extract_json_keys(line: &str) -> Vec<&str> {
    // Pull out `"key"` occurrences that are immediately followed by `:`.
    let mut keys = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'"' {
            // Find the closing quote.
            let start = i + 1;
            let mut j = start;
            while j < bytes.len() && bytes[j] != b'"' {
                if bytes[j] == b'\\' {
                    j += 1; // skip escaped char
                }
                j += 1;
            }
            if j < bytes.len() {
                // Check that the next non-space character is `:`.
                let after = &line[j + 1..].trim_start();
                if after.starts_with(':') {
                    if let Some(key) = line.get(start..j) {
                        keys.push(key);
                    }
                }
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }

    keys
}

fn parse_logfmt_field_hints(
    line: &str,
) -> Option<(Option<String>, Option<String>, Option<String>)> {
    let mut ts = None;
    let mut level = None;
    let mut msg = None;

    for token in line.split_whitespace() {
        if let Some(key) = token.split('=').next() {
            let lower = key.to_ascii_lowercase();
            if ts.is_none() && TIMESTAMP_KEYS.contains(&lower.as_str()) {
                ts = Some(key.to_string());
            } else if level.is_none() && LEVEL_KEYS.contains(&lower.as_str()) {
                level = Some(key.to_string());
            } else if msg.is_none() && MESSAGE_KEYS.contains(&lower.as_str()) {
                msg = Some(key.to_string());
            }
        }
    }

    if ts.is_some() || level.is_some() || msg.is_some() {
        Some((ts, level, msg))
    } else {
        None
    }
}

// ─── Timestamp pattern helpers ────────────────────────────────────────────────

fn looks_like_rfc3164_timestamp(line: &str) -> bool {
    // e.g. "Jan 12 08:15:00 hostname process[pid]: message"
    // Month name (3 chars), space, day (1–2 digits), space, HH:MM:SS
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    months.iter().any(|m| line.starts_with(m))
}

fn looks_like_rfc5424_timestamp(line: &str) -> bool {
    // RFC 5424 starts with `<priority>version timestamp`
    // Quick check: `<NNN>` at the start
    line.starts_with('<') && line.find('>').map_or(false, |pos| pos <= 4)
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
    fn test_detect_json_log() {
        let content = fixture("langchain_json.log");
        let format = detect(&content);
        assert_eq!(format.log_type, LogType::Json, "expected Json, got {:?}", format.log_type);
        // Should pick up common field names
        assert!(
            format.timestamp_field.as_deref() == Some("time"),
            "expected timestamp_field=time, got {:?}", format.timestamp_field
        );
        assert!(
            format.level_field.as_deref() == Some("level"),
            "expected level_field=level, got {:?}", format.level_field
        );
        assert!(
            format.message_field.as_deref() == Some("msg"),
            "expected message_field=msg, got {:?}", format.message_field
        );
    }

    #[test]
    fn test_detect_logfmt() {
        let content = fixture("crewai_logfmt.log");
        let format = detect(&content);
        assert_eq!(format.log_type, LogType::Logfmt, "expected Logfmt, got {:?}", format.log_type);
        assert!(format.timestamp_field.is_some(), "should detect timestamp field");
        assert!(format.level_field.is_some(), "should detect level field");
    }

    #[test]
    fn test_detect_plaintext() {
        let content = fixture("nginx_access.log");
        let format = detect(&content);
        // Nginx combined log is plain text (not JSON, Logfmt, or Syslog)
        assert_ne!(format.log_type, LogType::Json);
        assert_ne!(format.log_type, LogType::Logfmt);
    }

    #[test]
    fn test_detect_multiline() {
        let content = fixture("bedrock_multiline.log");
        let format = detect(&content);
        // Contains Python tracebacks — should score as Multiline or PlainText
        assert!(
            matches!(format.log_type, LogType::Multiline | LogType::PlainText),
            "expected Multiline or PlainText, got {:?}", format.log_type
        );
    }

    #[test]
    fn test_detect_empty() {
        let format = detect("");
        assert_eq!(format.log_type, LogType::PlainText);
    }

    #[test]
    fn test_detect_single_json_line() {
        let line = r#"{"time":"2026-01-01T00:00:00Z","level":"info","msg":"hello"}"#;
        let format = detect(line);
        assert_eq!(format.log_type, LogType::Json);
    }

    #[test]
    fn test_json_does_not_detect_logfmt() {
        let content = fixture("langchain_json.log");
        let format = detect(&content);
        assert_ne!(format.log_type, LogType::Logfmt);
    }
}
