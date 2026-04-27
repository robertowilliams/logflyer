//! Extracts a lightweight schema from structured (JSON or Logfmt) log samples.
//!
//! The extractor examines up to `max_lines` non-empty lines, collects every
//! key it finds, and computes per-field statistics:
//!
//! - **`presence_ratio`** — fraction of examined lines where the field appeared.
//! - **`inferred_type`** — the most common JSON value type seen for that field.
//! - **`is_identifier`** — heuristic flag for UUIDs / hashes / monotonic IDs.
//!
//! For non-structured formats (Syslog, PlainText, Multiline) the extractor
//! returns `None` so callers can omit the schema field from the metadata
//! document entirely.

use std::collections::HashMap;

use crate::models::{FieldInfo, FieldType, LogSchema, LogType};

/// Extract a [`LogSchema`] from `content` if the format supports it.
///
/// Returns `None` for non-structured formats.
pub fn extract(content: &str, log_type: &LogType, max_lines: usize) -> Option<LogSchema> {
    match log_type {
        LogType::Json => extract_json(content, max_lines),
        LogType::Logfmt => extract_logfmt(content, max_lines),
        _ => None,
    }
}

// ─── JSON schema extraction ───────────────────────────────────────────────────

fn extract_json(content: &str, max_lines: usize) -> Option<LogSchema> {
    let lines: Vec<&str> = content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && l.starts_with('{'))
        .take(max_lines)
        .collect();

    if lines.is_empty() {
        return None;
    }

    // field_name → list of (type_seen, present: bool) pairs
    let mut field_observations: HashMap<String, Vec<FieldType>> = HashMap::new();
    let mut parseable_count = 0_usize;

    for line in &lines {
        if let Some(fields) = parse_json_fields(line) {
            parseable_count += 1;
            for (key, ftype) in fields {
                field_observations.entry(key).or_default().push(ftype);
            }
        }
    }

    if parseable_count == 0 {
        return None;
    }

    let sample_coverage = parseable_count as f64 / lines.len() as f64;

    let mut field_infos: Vec<FieldInfo> = field_observations
        .into_iter()
        .map(|(name, observations)| {
            let presence_ratio = observations.len() as f64 / parseable_count as f64;
            let inferred_type = dominant_type(&observations);
            let is_identifier = looks_like_identifier(&name);
            FieldInfo {
                name,
                inferred_type,
                presence_ratio,
                is_identifier,
            }
        })
        .collect();

    // Sort by presence (most common first) then alphabetically.
    field_infos.sort_by(|a, b| {
        b.presence_ratio
            .partial_cmp(&a.presence_ratio)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.name.cmp(&b.name))
    });

    Some(LogSchema {
        fields: field_infos,
        sample_coverage,
    })
}

// ─── Logfmt schema extraction ─────────────────────────────────────────────────

fn extract_logfmt(content: &str, max_lines: usize) -> Option<LogSchema> {
    let lines: Vec<&str> = content
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('{') && l.contains('='))
        .take(max_lines)
        .collect();

    if lines.is_empty() {
        return None;
    }

    let mut field_observations: HashMap<String, Vec<FieldType>> = HashMap::new();
    let mut parseable_count = 0_usize;

    for line in &lines {
        let fields = parse_logfmt_fields(line);
        if !fields.is_empty() {
            parseable_count += 1;
            for (key, ftype) in fields {
                field_observations.entry(key).or_default().push(ftype);
            }
        }
    }

    if parseable_count == 0 {
        return None;
    }

    let sample_coverage = parseable_count as f64 / lines.len() as f64;

    let mut field_infos: Vec<FieldInfo> = field_observations
        .into_iter()
        .map(|(name, observations)| {
            let presence_ratio = observations.len() as f64 / parseable_count as f64;
            let inferred_type = dominant_type(&observations);
            let is_identifier = looks_like_identifier(&name);
            FieldInfo {
                name,
                inferred_type,
                presence_ratio,
                is_identifier,
            }
        })
        .collect();

    field_infos.sort_by(|a, b| {
        b.presence_ratio
            .partial_cmp(&a.presence_ratio)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.name.cmp(&b.name))
    });

    Some(LogSchema {
        fields: field_infos,
        sample_coverage,
    })
}

// ─── Minimal JSON field parser ────────────────────────────────────────────────
// We avoid pulling in `serde_json` here to keep this path allocation-light and
// purely synchronous.  The parser handles one level of top-level key/value
// pairs only — nested objects are typed as `Object` without recursion.

fn parse_json_fields(line: &str) -> Option<Vec<(String, FieldType)>> {
    // Must start with `{` and end with `}`.
    let inner = line.trim();
    if !inner.starts_with('{') || !inner.ends_with('}') {
        return None;
    }

    let mut fields = Vec::new();
    let content = &inner[1..inner.len() - 1];
    let mut chars = content.char_indices().peekable();

    loop {
        // Skip whitespace and commas.
        while let Some((_, ch)) = chars.peek() {
            if ch.is_whitespace() || *ch == ',' {
                chars.next();
            } else {
                break;
            }
        }

        // Expect a `"` to start the key.
        match chars.peek() {
            Some((_, '"')) => {
                chars.next(); // consume opening quote
            }
            _ => break,
        }

        // Read key until closing `"`.
        let mut key = String::new();
        let mut escaped = false;
        loop {
            match chars.next() {
                Some((_, '\\')) if !escaped => {
                    escaped = true;
                }
                Some((_, '"')) if !escaped => break,
                Some((_, ch)) => {
                    escaped = false;
                    key.push(ch);
                }
                None => return None,
            }
        }

        // Skip `:` and whitespace.
        while let Some((_, ch)) = chars.peek() {
            if ch.is_whitespace() || *ch == ':' {
                chars.next();
            } else {
                break;
            }
        }

        // Infer type from the first character of the value.
        let value_type = match chars.peek().map(|(_, ch)| *ch) {
            Some('"') => FieldType::String,
            Some('{') => FieldType::Object,
            Some('[') => FieldType::Array,
            Some('t') | Some('f') => FieldType::Bool,
            Some('n') => FieldType::Null,
            Some(c) if c.is_ascii_digit() || c == '-' => FieldType::Number,
            _ => FieldType::String, // fallback
        };

        // Consume the value token (skip until the next `,` at top level or `}`).
        skip_json_value(&mut chars);

        if !key.is_empty() {
            fields.push((key, value_type));
        }
    }

    if fields.is_empty() {
        None
    } else {
        Some(fields)
    }
}

fn skip_json_value(
    chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
) {
    let mut depth = 0_i32;
    let mut in_string = false;
    let mut escaped = false;

    loop {
        match chars.peek().map(|(_, ch)| *ch) {
            None => break,
            Some(ch) => {
                chars.next();

                if escaped {
                    escaped = false;
                    continue;
                }

                match ch {
                    '\\' if in_string => {
                        escaped = true;
                    }
                    '"' => {
                        in_string = !in_string;
                    }
                    '{' | '[' if !in_string => {
                        depth += 1;
                    }
                    '}' | ']' if !in_string => {
                        if depth == 0 {
                            break;
                        }
                        depth -= 1;
                    }
                    ',' if !in_string && depth == 0 => {
                        break;
                    }
                    _ => {}
                }
            }
        }
    }
}

// ─── Logfmt field parser ──────────────────────────────────────────────────────

fn parse_logfmt_fields(line: &str) -> Vec<(String, FieldType)> {
    let mut fields = Vec::new();

    for token in line.split_whitespace() {
        if let Some((key, value)) = token.split_once('=') {
            if key.is_empty() {
                continue;
            }
            let ftype = infer_logfmt_type(value);
            fields.push((key.to_string(), ftype));
        }
    }

    fields
}

fn infer_logfmt_type(value: &str) -> FieldType {
    if value.is_empty() || value == "null" {
        return FieldType::Null;
    }
    if value == "true" || value == "false" {
        return FieldType::Bool;
    }
    if value.parse::<f64>().is_ok() {
        return FieldType::Number;
    }
    FieldType::String
}

// ─── Utilities ────────────────────────────────────────────────────────────────

fn dominant_type(types: &[FieldType]) -> FieldType {
    // Return the most frequently observed type; default to String on a tie.
    let mut counts = [0_usize; 6]; // index matches FieldType discriminant order

    for t in types {
        let idx = match t {
            FieldType::String => 0,
            FieldType::Number => 1,
            FieldType::Bool => 2,
            FieldType::Object => 3,
            FieldType::Array => 4,
            FieldType::Null => 5,
        };
        counts[idx] += 1;
    }

    let max_idx = counts
        .iter()
        .enumerate()
        .max_by_key(|(_, &v)| v)
        .map(|(i, _)| i)
        .unwrap_or(0);

    match max_idx {
        0 => FieldType::String,
        1 => FieldType::Number,
        2 => FieldType::Bool,
        3 => FieldType::Object,
        4 => FieldType::Array,
        _ => FieldType::Null,
    }
}

/// Return `true` when a field name is a good candidate for a unique identifier.
fn looks_like_identifier(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let id_suffixes = ["_id", "_uuid", "_hash", "_key", "id", "uuid", "trace_id",
                       "span_id", "request_id", "correlation_id", "transaction_id"];
    id_suffixes.iter().any(|suffix| lower == *suffix || lower.ends_with(suffix))
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
    fn test_json_schema_extracted() {
        let content = fixture("langchain_json.log");
        let schema = extract(&content, &LogType::Json, 200);

        assert!(schema.is_some(), "should extract schema from JSON log");
        let schema = schema.unwrap();

        assert!(!schema.fields.is_empty(), "should have fields");
        assert!(schema.sample_coverage > 0.5, "coverage should be >50%");

        // "time", "level", "msg" appear in every line
        let names: Vec<&str> = schema.fields.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"time"), "should include 'time' field");
        assert!(names.contains(&"level"), "should include 'level' field");
        assert!(names.contains(&"msg"), "should include 'msg' field");
    }

    #[test]
    fn test_json_field_types() {
        let content = fixture("langchain_json.log");
        let schema = extract(&content, &LogType::Json, 200).unwrap();

        let level_field = schema.fields.iter().find(|f| f.name == "level");
        assert!(level_field.is_some());
        assert_eq!(level_field.unwrap().inferred_type, FieldType::String);
    }

    #[test]
    fn test_json_identifier_detection() {
        let content = fixture("langchain_json.log");
        let schema = extract(&content, &LogType::Json, 200).unwrap();

        // agent_id and session_id should be flagged as identifiers
        let id_fields: Vec<&str> = schema
            .fields
            .iter()
            .filter(|f| f.is_identifier)
            .map(|f| f.name.as_str())
            .collect();
        assert!(
            id_fields.iter().any(|n| n.contains("id")),
            "should flag _id fields as identifiers, got {:?}", id_fields
        );
    }

    #[test]
    fn test_logfmt_schema_extracted() {
        let content = fixture("crewai_logfmt.log");
        let schema = extract(&content, &LogType::Logfmt, 200);

        assert!(schema.is_some(), "should extract schema from Logfmt log");
        let schema = schema.unwrap();
        assert!(!schema.fields.is_empty());

        let names: Vec<&str> = schema.fields.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"level"), "logfmt fixture should have 'level'");
        assert!(names.contains(&"msg"), "logfmt fixture should have 'msg'");
    }

    #[test]
    fn test_plain_text_returns_none() {
        let content = fixture("nginx_access.log");
        let schema = extract(&content, &LogType::PlainText, 200);
        assert!(schema.is_none(), "plain text should return None schema");
    }

    #[test]
    fn test_syslog_returns_none() {
        let content = fixture("bedrock_multiline.log");
        let schema = extract(&content, &LogType::Syslog, 200);
        assert!(schema.is_none());
    }

    #[test]
    fn test_presence_ratio_bounds() {
        let content = fixture("langchain_json.log");
        let schema = extract(&content, &LogType::Json, 200).unwrap();
        for field in &schema.fields {
            assert!(
                field.presence_ratio > 0.0 && field.presence_ratio <= 1.0,
                "presence_ratio out of range for field '{}': {}",
                field.name,
                field.presence_ratio
            );
        }
    }

    #[test]
    fn test_max_lines_respected() {
        let content = fixture("langchain_json.log");
        // Extract with max 3 lines — should still work, just with low coverage
        let schema = extract(&content, &LogType::Json, 3);
        assert!(schema.is_some());
        let schema = schema.unwrap();
        assert!(schema.sample_coverage <= 1.0);
    }
}
