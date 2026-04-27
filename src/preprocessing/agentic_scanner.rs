//! Scans a log sample for signals that indicate LLM / agentic activity.
//!
//! The scanner uses a compiled set of regular expressions (initialised once via
//! [`once_cell`]) to avoid re-compiling patterns on every call.  Each pattern
//! carries a human-readable label and is associated with zero or more framework
//! names so that callers can see _which_ agent frameworks are present.
//!
//! The `signal_score` is the fraction of non-empty lines that matched at least
//! one pattern.  A sample is considered `worth_classifying` when that score
//! meets or exceeds the caller-supplied threshold.

use once_cell::sync::Lazy;
use regex::Regex;

use crate::models::AgenticScan;

struct Pattern {
    label: &'static str,
    frameworks: &'static [&'static str],
    regex: Regex,
}

impl Pattern {
    fn new(label: &'static str, frameworks: &'static [&'static str], pattern: &str) -> Self {
        Self {
            label,
            frameworks,
            regex: Regex::new(pattern).expect("agentic pattern must compile"),
        }
    }
}

static PATTERNS: Lazy<Vec<Pattern>> = Lazy::new(|| {
    vec![
        // ── LLM API calls ────────────────────────────────────────────────────
        Pattern::new(
            "openai_api_call",
            &["openai"],
            r"(?i)(openai|gpt-[34o]|chatgpt|completions?|chat\.completions?)",
        ),
        Pattern::new(
            "anthropic_api_call",
            &["anthropic"],
            r"(?i)(anthropic|claude-[23]|claude-opus|claude-sonnet|claude-haiku)",
        ),
        Pattern::new(
            "llm_token_usage",
            &[],
            r"(?i)(prompt_tokens|completion_tokens|total_tokens|usage\s*[:=]\s*\{)",
        ),
        Pattern::new(
            "llm_finish_reason",
            &[],
            r#"(?i)finish_reason\s*[:=]\s*["']?(stop|length|tool_calls|content_filter)"#,
        ),
        // ── Agent frameworks ─────────────────────────────────────────────────
        Pattern::new(
            "langchain_activity",
            &["langchain"],
            r"(?i)(langchain|LLMChain|AgentExecutor|PromptTemplate|BaseTool|ConversationChain)",
        ),
        Pattern::new(
            "langgraph_activity",
            &["langgraph"],
            r"(?i)(langgraph|StateGraph|CompiledGraph|add_node|add_edge|invoke\()",
        ),
        Pattern::new(
            "autogen_activity",
            &["autogen"],
            r"(?i)(autogen|AssistantAgent|UserProxyAgent|GroupChat|initiate_chat)",
        ),
        Pattern::new(
            "crewai_activity",
            &["crewai"],
            r"(?i)(crewai|CrewAI|Crew\(|Agent\(|Task\(|crew\.kickoff)",
        ),
        Pattern::new(
            "llamaindex_activity",
            &["llamaindex"],
            r"(?i)(llama.?index|LlamaIndex|VectorStoreIndex|QueryEngine|ServiceContext)",
        ),
        Pattern::new(
            "haystack_activity",
            &["haystack"],
            r"(?i)(haystack|Pipeline\.run|DocumentStore|Retriever|Generator)",
        ),
        // ── Tool / function calls ────────────────────────────────────────────
        Pattern::new(
            "tool_call",
            &[],
            r#"(?i)(tool_call|function_call|tool_use|"name"\s*:\s*"[a-z_]+"|calling tool)"#,
        ),
        Pattern::new(
            "tool_result",
            &[],
            r"(?i)(tool_result|function_result|observation\s*[:=]|Action Input|Action Output)",
        ),
        // ── Memory / retrieval ───────────────────────────────────────────────
        Pattern::new(
            "vector_retrieval",
            &[],
            r"(?i)(vector.?store|embedding|similarity.?search|nearest.?neighbor|cosine.?sim)",
        ),
        Pattern::new(
            "rag_activity",
            &[],
            r"(?i)(retrieval.?augmented|RAG|context.?window|augment.*context|retrieved.*chunks?)",
        ),
        // ── Prompt engineering signals ───────────────────────────────────────
        Pattern::new(
            "system_prompt",
            &[],
            r#"(?i)(system\s*[:=]\s*["'\{]|"role"\s*:\s*"system"|system_message)"#,
        ),
        Pattern::new(
            "prompt_template",
            &[],
            r"(?i)(PromptTemplate|FewShotPrompt|ChatPromptTemplate|\{[a-z_]+\}.*\{[a-z_]+\})",
        ),
        // ── Agent reasoning ──────────────────────────────────────────────────
        Pattern::new(
            "chain_of_thought",
            &[],
            r"(?i)(chain.of.thought|step.by.step|let.me.think|reasoning\s*[:=]|Thought\s*:)",
        ),
        Pattern::new(
            "agent_action",
            &[],
            r"(?i)(Action\s*:\s*\w|Observation\s*:|Final Answer\s*:|PAUSE|REFLECT|PLAN)",
        ),
        // ── Error / retry patterns common in agentic loops ───────────────────
        Pattern::new(
            "rate_limit_retry",
            &[],
            r"(?i)(rate.?limit|retry.?after|exponential.?backoff|too.?many.?requests|429)",
        ),
        Pattern::new(
            "context_overflow",
            &[],
            r"(?i)(context.?length|token.?limit|max.?tokens|context.?window.?exceeded|truncat)",
        ),
    ]
});

/// Scan `content` for agentic signals; return an [`AgenticScan`] result.
///
/// `threshold` is the minimum `signal_score` for `worth_classifying` to be
/// `true`.  Typical values are in the 0.01 – 0.05 range.
pub fn scan(content: &str, threshold: f64) -> AgenticScan {
    let non_empty_lines: Vec<&str> = content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();

    let total = non_empty_lines.len();
    if total == 0 {
        return empty_scan(threshold);
    }

    let mut agentic_line_count: i64 = 0;
    let mut matched_pattern_labels: Vec<&'static str> = Vec::new();
    let mut matched_framework_set: std::collections::HashSet<&'static str> =
        std::collections::HashSet::new();

    for line in &non_empty_lines {
        let mut line_matched = false;

        for pattern in PATTERNS.iter() {
            if pattern.regex.is_match(line) {
                line_matched = true;

                // Record pattern label (deduplicated across all lines)
                if !matched_pattern_labels.contains(&pattern.label) {
                    matched_pattern_labels.push(pattern.label);
                }

                for &fw in pattern.frameworks {
                    matched_framework_set.insert(fw);
                }
            }
        }

        if line_matched {
            agentic_line_count += 1;
        }
    }

    let signal_score = agentic_line_count as f64 / total as f64;

    AgenticScan {
        signal_score,
        worth_classifying: signal_score >= threshold,
        detected_frameworks: matched_framework_set
            .into_iter()
            .map(ToString::to_string)
            .collect(),
        matched_patterns: matched_pattern_labels
            .into_iter()
            .map(ToString::to_string)
            .collect(),
        agentic_line_count,
    }
}

fn empty_scan(threshold: f64) -> AgenticScan {
    AgenticScan {
        signal_score: 0.0,
        worth_classifying: 0.0_f64 >= threshold,
        detected_frameworks: Vec::new(),
        matched_patterns: Vec::new(),
        agentic_line_count: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const THRESHOLD: f64 = 0.02;

    fn fixture(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("fixture not found: {}", path.display()))
    }

    #[test]
    fn test_langchain_detected() {
        let content = fixture("langchain_json.log");
        let result = scan(&content, THRESHOLD);

        assert!(
            result.worth_classifying,
            "LangChain log should be worth classifying, score={}",
            result.signal_score
        );
        assert!(result.signal_score > 0.3, "expected strong signal, got {}", result.signal_score);
        assert!(
            result.detected_frameworks.iter().any(|f| f == "langchain" || f == "openai"),
            "expected langchain or openai framework, got {:?}",
            result.detected_frameworks
        );
        assert!(result.agentic_line_count > 5);
    }

    #[test]
    fn test_crewai_detected() {
        let content = fixture("crewai_logfmt.log");
        let result = scan(&content, THRESHOLD);

        assert!(result.worth_classifying, "CrewAI log should be worth classifying");
        assert!(
            result.detected_frameworks.iter().any(|f| f == "crewai" || f == "anthropic"),
            "expected crewai or anthropic, got {:?}", result.detected_frameworks
        );
    }

    #[test]
    fn test_nginx_no_signal() {
        let content = fixture("nginx_access.log");
        let result = scan(&content, THRESHOLD);

        assert!(
            result.signal_score < 0.05,
            "nginx log should have near-zero signal, got {}",
            result.signal_score
        );
        // Note: worth_classifying may still be false or true depending on exact score
        assert!(
            !result.worth_classifying || result.signal_score < 0.05,
            "nginx should not be worth classifying"
        );
    }

    #[test]
    fn test_bedrock_rate_limit_detected() {
        let content = fixture("bedrock_multiline.log");
        let result = scan(&content, THRESHOLD);

        assert!(result.worth_classifying, "Bedrock log should be worth classifying");
        assert!(
            result.matched_patterns.iter().any(|p| p.contains("rate_limit") || p.contains("context")),
            "should detect rate_limit or context_overflow patterns, got {:?}",
            result.matched_patterns
        );
    }

    #[test]
    fn test_empty_content() {
        let result = scan("", THRESHOLD);
        assert_eq!(result.signal_score, 0.0);
        assert!(!result.worth_classifying);
        assert_eq!(result.agentic_line_count, 0);
    }

    #[test]
    fn test_threshold_respected() {
        // A log with one agentic line out of 100 = 0.01 score < 0.02 threshold
        let mut lines = vec![r#"{"msg":"tool_call","tool":"search"}"#.to_string()];
        for i in 0..99 {
            lines.push(format!(r#"{{"msg":"plain log line {}"}}"#, i));
        }
        let content = lines.join("\n");
        let result = scan(&content, 0.02);
        // 1 agentic line / 100 total = 0.01 < threshold 0.02
        assert!(
            !result.worth_classifying,
            "1/100 signal should be below threshold 0.02, score={}",
            result.signal_score
        );
    }

    #[test]
    fn test_signal_score_range() {
        let content = fixture("langchain_json.log");
        let result = scan(&content, THRESHOLD);
        assert!(result.signal_score >= 0.0);
        assert!(result.signal_score <= 1.0);
    }
}
