use crate::models::{PromptTemplate, SampleMetadata, SampleRecord};

const RESPONSE_SCHEMA: &str = r#"Respond ONLY with valid JSON — no markdown, no commentary — matching this exact schema:
{
  "severity":        "critical|warning|info|normal",
  "categories":      ["error","anomaly","performance","security","..."],
  "summary":         "1-3 sentences describing what you found",
  "key_findings":    [{"pattern":"...","count":0,"severity":"critical|warning|info","example":"verbatim log line"}],
  "recommendations": ["actionable suggestion"],
  "confidence":      0.85
}"#;

/// Build the (system, user) prompt pair for the given sample + metadata.
pub fn build(record: &SampleRecord, metadata: &SampleMetadata) -> (String, String) {
    let template = &metadata.ingestion_hints.prompt_template;
    let system   = system_prompt(template);
    let user     = user_prompt(record, metadata);
    (system, user)
}

fn system_prompt(template: &PromptTemplate) -> String {
    let role = match template {
        PromptTemplate::JsonAgent => {
            "You are an expert log analysis system specialising in structured JSON logs. \
             You detect schema violations, unexpected null fields, latency spikes, error \
             rate anomalies, and signs of framework-level failures (e.g. LangChain, OpenAI SDK)."
        }
        PromptTemplate::LogfmtAgent => {
            "You are an expert log analysis system specialising in logfmt-formatted logs. \
             You parse key=value pairs to detect errors, retries, high latencies, auth \
             failures, and service degradation patterns."
        }
        PromptTemplate::Syslog => {
            "You are an expert log analysis system specialising in syslog-format logs. \
             You identify kernel panics, OOM events, cron failures, systemd service \
             crashes, and facility/severity escalations."
        }
        PromptTemplate::Generic => {
            "You are an expert log analysis system. You identify errors, warnings, \
             anomalies, recurring failure patterns, and operational issues in raw log output."
        }
    };

    format!("{role}\n\n{RESPONSE_SCHEMA}")
}

fn user_prompt(record: &SampleRecord, metadata: &SampleMetadata) -> String {
    let mut parts = Vec::new();

    parts.push(format!("## Log sample\n\nTarget: {}\nHost: {}\nFile: {}",
        record.target_id, record.host, record.source_file));

    // Preprocessing context — gives the model a head start on what to look for.
    if !metadata.agentic_scan.matched_patterns.is_empty() {
        parts.push(format!(
            "## Preprocessor signals\n\nMatched patterns: {}\nSignal score: {:.4}",
            metadata.agentic_scan.matched_patterns.join(", "),
            metadata.agentic_scan.signal_score,
        ));
    }

    if !metadata.agentic_scan.detected_frameworks.is_empty() {
        parts.push(format!(
            "Detected frameworks: {}",
            metadata.agentic_scan.detected_frameworks.join(", ")
        ));
    }

    // Truncate the sample content to the suggested chunk size.
    let chunk_size = metadata.ingestion_hints.suggested_chunk_size as usize;
    let content    = truncate_to_lines(&record.sample_content, chunk_size);

    parts.push(format!("## Content\n\n{content}"));
    parts.push("Analyse this log sample and return your findings as JSON.".to_string());

    parts.join("\n\n")
}

/// Keep at most `max_lines` lines from the sample, preserving the HEAD / TAIL
/// boundary markers when they are present.
fn truncate_to_lines(content: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = content.lines().collect();
    if lines.len() <= max_lines {
        return content.to_string();
    }
    // Keep first half from HEAD and second half from TAIL.
    let half   = max_lines / 2;
    let head   = &lines[..half];
    let tail   = &lines[lines.len() - half..];
    format!("{}\n[... truncated ...]\n{}", head.join("\n"), tail.join("\n"))
}
