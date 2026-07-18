//! Stage 8 — build directed [`RelationEdge`] records between entities.
//!
//! Applies 8 structural inference rules over the typed, classified entity list
//! produced by Stages 6–7.  Entities must be ordered by `line_index` ascending
//! (the order returned by [`super::entity_extractor::extract`]).
//!
//! # Rules
//!
//! | # | Pattern | Edge type | Confidence | Source |
//! |---|---------|-----------|------------|--------|
//! | 1 | CompletionEvent ← nearest preceding PromptEvent | RespondedTo | 0.7 | Inferred |
//! | 2 | ToolCallEvent ← nearest preceding AgentStep | TriggeredBy | 0.7 | Inferred |
//! | 3 | ToolCallEvent → ToolResultEvent (same tool_name) | Generated | 1.0 | Explicit |
//! | 3b| ToolCallEvent → nearest subsequent ToolResultEvent | Generated | 0.7 | Inferred |
//! | 4 | RetrievalEvent → nearest subsequent PromptEvent | Informed | 0.7 | Inferred |
//! | 5 | AgentStep(n) → AgentStep(n+1) | FollowedBy | 0.7 | Inferred |
//! | 6 | AgentStep → nearest subsequent McpEvent | DelegatedTo | 0.7 | Inferred |
//! | 7 | ContextWindow → each preceding non-CW entity | AssembledFrom | 1.0 | Inferred |
//! | 8 | Every entity → its OTel trace_id | PartOf | 1.0 | Inferred |

use mongodb::bson::DateTime;

use crate::models::{EntityRecord, EntityType, RelationEdge, RelationSource, RelationType};
use super::ids;

// ─── Edge constructor ─────────────────────────────────────────────────────────

fn make_edge(
    relation_type: RelationType,
    source_entity_id: &str,
    target_entity_id: &str,
    sample_hash: &str,
    confidence: f32,
    source: RelationSource,
) -> RelationEdge {
    // Content-derived relation_id keyed on (sample_hash, type, src, dst) so
    // upsert filters in `output::graph::write_edges` actually match on re-run.
    // `Debug` rendering is used for the type discriminator — stable across
    // builds and changes only when an enum variant is renamed (which is a
    // breaking change requiring a backfill anyway).
    let relation_id = ids::derive_relation_id(
        sample_hash,
        &format!("{:?}", relation_type),
        source_entity_id,
        target_entity_id,
    );
    RelationEdge {
        relation_id,
        relation_type,
        source_entity_id: source_entity_id.to_string(),
        target_entity_id: target_entity_id.to_string(),
        sample_hash: sample_hash.to_string(),
        confidence,
        source,
        created_at: DateTime::now(),
    }
}

// ─── Public entry point ───────────────────────────────────────────────────────

/// Infer directed [`RelationEdge`] records from a typed entity list.
///
/// Entities are assumed to be ordered by `line_index` ascending (the default
/// ordering produced by [`super::entity_extractor::extract`]).  The function
/// is pure — it does not mutate its inputs — and is safe to call multiple times.
///
/// # Arguments
/// * `entities`    – typed + classified entities from Stages 6–7
/// * `sample_hash` – stable hash of the parent [`crate::models::SampleRecord`]
pub fn extract(entities: &[EntityRecord], sample_hash: &str) -> Vec<RelationEdge> {
    let mut edges: Vec<RelationEdge> = Vec::new();

    // Apply rules in dependency order: cheapest / most universal first.
    emit_part_of(entities, sample_hash, &mut edges);        // Rule 8
    emit_assembled_from(entities, sample_hash, &mut edges); // Rule 7
    emit_followed_by(entities, sample_hash, &mut edges);    // Rule 5
    emit_responded_to(entities, sample_hash, &mut edges);   // Rule 1
    emit_triggered_by(entities, sample_hash, &mut edges);   // Rule 2
    emit_generated(entities, sample_hash, &mut edges);      // Rule 3
    emit_informed(entities, sample_hash, &mut edges);       // Rule 4
    emit_delegated_to(entities, sample_hash, &mut edges);   // Rule 6

    edges
}

// ─── Rule 8: PartOf ───────────────────────────────────────────────────────────

/// Every entity belongs to the OTel trace that spans its sample.
///
/// `entity --PartOf--> trace_id`
///
/// Note: `target_entity_id` holds the 32-hex-char trace ID string rather than
/// an `entity_id`, as the trace is not itself an entity record.
fn emit_part_of(entities: &[EntityRecord], sample_hash: &str, edges: &mut Vec<RelationEdge>) {
    for e in entities {
        edges.push(make_edge(
            RelationType::PartOf,
            &e.entity_id,
            &e.trace_id,
            sample_hash,
            1.0,
            RelationSource::Inferred,
        ));
    }
}

// ─── Rule 7: AssembledFrom ────────────────────────────────────────────────────

/// Each ContextWindow entity was assembled from every non-ContextWindow entity
/// that precedes it in the log.
///
/// `context_window --AssembledFrom--> contributing_entity`
fn emit_assembled_from(
    entities: &[EntityRecord],
    sample_hash: &str,
    edges: &mut Vec<RelationEdge>,
) {
    for (i, cw) in entities.iter().enumerate() {
        if cw.entity_type != EntityType::ContextWindow {
            continue;
        }
        for contrib in entities[..i]
            .iter()
            .filter(|e| e.entity_type != EntityType::ContextWindow)
        {
            edges.push(make_edge(
                RelationType::AssembledFrom,
                &cw.entity_id,
                &contrib.entity_id,
                sample_hash,
                1.0,
                RelationSource::Inferred,
            ));
        }
    }
}

// ─── Rule 5: FollowedBy ───────────────────────────────────────────────────────

/// Consecutive AgentStep entities in document order form a temporal chain.
///
/// `agent_step_n --FollowedBy--> agent_step_n+1`
fn emit_followed_by(entities: &[EntityRecord], sample_hash: &str, edges: &mut Vec<RelationEdge>) {
    let steps: Vec<&EntityRecord> = entities
        .iter()
        .filter(|e| e.entity_type == EntityType::AgentStep)
        .collect();

    for pair in steps.windows(2) {
        edges.push(make_edge(
            RelationType::FollowedBy,
            &pair[0].entity_id,
            &pair[1].entity_id,
            sample_hash,
            0.7,
            RelationSource::Inferred,
        ));
    }
}

// ─── Rule 1: RespondedTo ─────────────────────────────────────────────────────

/// A CompletionEvent responded to the nearest preceding PromptEvent.
///
/// `completion --RespondedTo--> prompt`
fn emit_responded_to(
    entities: &[EntityRecord],
    sample_hash: &str,
    edges: &mut Vec<RelationEdge>,
) {
    for (i, e) in entities.iter().enumerate() {
        if e.entity_type != EntityType::CompletionEvent {
            continue;
        }
        if let Some(prompt) = entities[..i]
            .iter()
            .rev()
            .find(|p| p.entity_type == EntityType::PromptEvent)
        {
            edges.push(make_edge(
                RelationType::RespondedTo,
                &e.entity_id,
                &prompt.entity_id,
                sample_hash,
                0.7,
                RelationSource::Inferred,
            ));
        }
    }
}

// ─── Rule 2: TriggeredBy ─────────────────────────────────────────────────────

/// A ToolCallEvent was triggered by the nearest preceding AgentStep.
///
/// `tool_call --TriggeredBy--> agent_step`
fn emit_triggered_by(
    entities: &[EntityRecord],
    sample_hash: &str,
    edges: &mut Vec<RelationEdge>,
) {
    for (i, e) in entities.iter().enumerate() {
        if e.entity_type != EntityType::ToolCallEvent {
            continue;
        }
        if let Some(agent) = entities[..i]
            .iter()
            .rev()
            .find(|a| a.entity_type == EntityType::AgentStep)
        {
            edges.push(make_edge(
                RelationType::TriggeredBy,
                &e.entity_id,
                &agent.entity_id,
                sample_hash,
                0.7,
                RelationSource::Inferred,
            ));
        }
    }
}

// ─── Rule 3: Generated ───────────────────────────────────────────────────────

/// A ToolCallEvent generated a ToolResultEvent.
///
/// Match by `tool_name` (Explicit, confidence 1.0) when available; fall back
/// to the nearest subsequent ToolResultEvent (Inferred, confidence 0.7).
///
/// `tool_call --Generated--> tool_result`
fn emit_generated(entities: &[EntityRecord], sample_hash: &str, edges: &mut Vec<RelationEdge>) {
    for (i, call) in entities.iter().enumerate() {
        if call.entity_type != EntityType::ToolCallEvent {
            continue;
        }

        let rest = &entities[i + 1..];

        // Try explicit tool_name match first.
        let explicit = call.tool_name.as_deref().and_then(|call_name| {
            rest.iter().find(|r| {
                r.entity_type == EntityType::ToolResultEvent
                    && r.tool_name.as_deref() == Some(call_name)
            })
        });

        if let Some(result) = explicit {
            edges.push(make_edge(
                RelationType::Generated,
                &call.entity_id,
                &result.entity_id,
                sample_hash,
                1.0,
                RelationSource::Explicit,
            ));
        } else if let Some(result) = rest
            .iter()
            .find(|r| r.entity_type == EntityType::ToolResultEvent)
        {
            edges.push(make_edge(
                RelationType::Generated,
                &call.entity_id,
                &result.entity_id,
                sample_hash,
                0.7,
                RelationSource::Inferred,
            ));
        }
    }
}

// ─── Rule 4: Informed ────────────────────────────────────────────────────────

/// A RetrievalEvent informed the nearest subsequent PromptEvent.
///
/// `retrieval --Informed--> prompt`
fn emit_informed(entities: &[EntityRecord], sample_hash: &str, edges: &mut Vec<RelationEdge>) {
    for (i, e) in entities.iter().enumerate() {
        if e.entity_type != EntityType::RetrievalEvent {
            continue;
        }
        if let Some(prompt) = entities[i + 1..]
            .iter()
            .find(|p| p.entity_type == EntityType::PromptEvent)
        {
            edges.push(make_edge(
                RelationType::Informed,
                &e.entity_id,
                &prompt.entity_id,
                sample_hash,
                0.7,
                RelationSource::Inferred,
            ));
        }
    }
}

// ─── Rule 6: DelegatedTo ─────────────────────────────────────────────────────

/// An AgentStep delegated work to the nearest subsequent McpEvent.
///
/// `agent_step --DelegatedTo--> mcp_event`
fn emit_delegated_to(
    entities: &[EntityRecord],
    sample_hash: &str,
    edges: &mut Vec<RelationEdge>,
) {
    for (i, e) in entities.iter().enumerate() {
        if e.entity_type != EntityType::AgentStep {
            continue;
        }
        if let Some(mcp) = entities[i + 1..]
            .iter()
            .find(|m| m.entity_type == EntityType::McpEvent)
        {
            edges.push(make_edge(
                RelationType::DelegatedTo,
                &e.entity_id,
                &mcp.entity_id,
                sample_hash,
                0.7,
                RelationSource::Inferred,
            ));
        }
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::models::{EntityType, SemanticRole};

    // ── Entity builder ────────────────────────────────────────────────────────

    fn make_entity(entity_type: EntityType, line_index: u32) -> EntityRecord {
        make_entity_with_tool(entity_type, line_index, None)
    }

    fn make_entity_with_tool(
        entity_type: EntityType,
        line_index: u32,
        tool_name: Option<&str>,
    ) -> EntityRecord {
        EntityRecord {
            entity_id: format!("eid-{line_index}"),
            entity_type,
            semantic_role: SemanticRole::Unknown,
            sample_hash: "testhash".to_string(),
            target_id: "t1".to_string(),
            trace_id: "aabbccdd00112233aabbccdd00112233".to_string(),
            span_id: format!("span{line_index:04}"),
            parent_span_id: None,
            prov_entity_id: format!("ug:entity:eid-{line_index}"),
            prov_activity_id: format!("ug:activity:testhash:{line_index}"),
            line_index,
            raw_text: String::new(),
            extracted_fields: HashMap::new(),
            model_id: None,
            tool_name: tool_name.map(str::to_string),
            mcp_server_id: None,
            token_count: None,
            latency_ms: None,
            timestamp_utc: None,
            content_embedding_id: None,
            behavioral_embedding_id: None,
        }
    }

    // ── Helper: count edges by type ───────────────────────────────────────────

    fn count(edges: &[RelationEdge], rt: &RelationType) -> usize {
        edges.iter().filter(|e| &e.relation_type == rt).count()
    }

    fn find_edge<'a>(
        edges: &'a [RelationEdge],
        rt: &RelationType,
        src: &str,
        tgt: &str,
    ) -> Option<&'a RelationEdge> {
        edges.iter().find(|e| {
            &e.relation_type == rt && e.source_entity_id == src && e.target_entity_id == tgt
        })
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Rule 8: PartOf
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn part_of_emitted_for_every_entity() {
        let entities = vec![
            make_entity(EntityType::PromptEvent, 0),
            make_entity(EntityType::CompletionEvent, 1),
            make_entity(EntityType::ToolCallEvent, 2),
        ];
        let edges = extract(&entities, "h");
        let part_of: Vec<_> = edges
            .iter()
            .filter(|e| e.relation_type == RelationType::PartOf)
            .collect();
        assert_eq!(part_of.len(), 3, "one PartOf per entity");
        for (i, edge) in part_of.iter().enumerate() {
            assert_eq!(
                edge.source_entity_id, entities[i].entity_id,
                "source is entity"
            );
            assert_eq!(
                edge.target_entity_id,
                "aabbccdd00112233aabbccdd00112233",
                "target is trace_id"
            );
            assert!((edge.confidence - 1.0).abs() < f32::EPSILON);
            assert_eq!(edge.source, RelationSource::Inferred);
        }
    }

    #[test]
    fn part_of_empty_input() {
        let edges = extract(&[], "h");
        assert!(edges.is_empty());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Rule 7: AssembledFrom
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn assembled_from_links_context_window_to_preceding_entities() {
        let entities = vec![
            make_entity(EntityType::PromptEvent, 0),
            make_entity(EntityType::RetrievalEvent, 1),
            make_entity(EntityType::ContextWindow, 2),
        ];
        let edges = extract(&entities, "h");
        let af: Vec<_> = edges
            .iter()
            .filter(|e| e.relation_type == RelationType::AssembledFrom)
            .collect();
        // ContextWindow (line 2) ← PromptEvent (0) and RetrievalEvent (1)
        assert_eq!(af.len(), 2);
        assert!(af.iter().all(|e| e.source_entity_id == "eid-2"));
        let targets: std::collections::HashSet<&str> =
            af.iter().map(|e| e.target_entity_id.as_str()).collect();
        assert!(targets.contains("eid-0"));
        assert!(targets.contains("eid-1"));
        assert!(af.iter().all(|e| (e.confidence - 1.0).abs() < f32::EPSILON));
    }

    #[test]
    fn assembled_from_skips_other_context_windows() {
        let entities = vec![
            make_entity(EntityType::ContextWindow, 0),
            make_entity(EntityType::PromptEvent, 1),
            make_entity(EntityType::ContextWindow, 2),
        ];
        let edges = extract(&entities, "h");
        let af: Vec<_> = edges
            .iter()
            .filter(|e| e.relation_type == RelationType::AssembledFrom)
            .collect();
        // Second CW (eid-2) ← PromptEvent (eid-1) only; first CW excluded
        assert_eq!(af.len(), 1);
        assert_eq!(af[0].source_entity_id, "eid-2");
        assert_eq!(af[0].target_entity_id, "eid-1");
    }

    #[test]
    fn assembled_from_no_preceding_entities() {
        let entities = vec![make_entity(EntityType::ContextWindow, 0)];
        let edges = extract(&entities, "h");
        assert_eq!(count(&edges, &RelationType::AssembledFrom), 0);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Rule 5: FollowedBy
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn followed_by_chains_agent_steps() {
        let entities = vec![
            make_entity(EntityType::AgentStep, 0),
            make_entity(EntityType::PromptEvent, 1), // non-AgentStep interleaved
            make_entity(EntityType::AgentStep, 2),
            make_entity(EntityType::AgentStep, 3),
        ];
        let edges = extract(&entities, "h");
        let fb: Vec<_> = edges
            .iter()
            .filter(|e| e.relation_type == RelationType::FollowedBy)
            .collect();
        // eid-0 → eid-2, eid-2 → eid-3
        assert_eq!(fb.len(), 2);
        assert!(find_edge(&edges, &RelationType::FollowedBy, "eid-0", "eid-2").is_some());
        assert!(find_edge(&edges, &RelationType::FollowedBy, "eid-2", "eid-3").is_some());
        assert!(fb.iter().all(|e| (e.confidence - 0.7).abs() < f32::EPSILON));
    }

    #[test]
    fn followed_by_single_agent_step_no_edge() {
        let entities = vec![make_entity(EntityType::AgentStep, 0)];
        let edges = extract(&entities, "h");
        assert_eq!(count(&edges, &RelationType::FollowedBy), 0);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Rule 1: RespondedTo
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn responded_to_completion_links_nearest_prompt() {
        let entities = vec![
            make_entity(EntityType::PromptEvent, 0),
            make_entity(EntityType::PromptEvent, 1),
            make_entity(EntityType::CompletionEvent, 2),
        ];
        let edges = extract(&entities, "h");
        let rt: Vec<_> = edges
            .iter()
            .filter(|e| e.relation_type == RelationType::RespondedTo)
            .collect();
        assert_eq!(rt.len(), 1);
        // nearest preceding prompt is eid-1
        assert_eq!(rt[0].source_entity_id, "eid-2");
        assert_eq!(rt[0].target_entity_id, "eid-1");
        assert!((rt[0].confidence - 0.7).abs() < f32::EPSILON);
        assert_eq!(rt[0].source, RelationSource::Inferred);
    }

    #[test]
    fn responded_to_no_preceding_prompt_no_edge() {
        let entities = vec![make_entity(EntityType::CompletionEvent, 0)];
        let edges = extract(&entities, "h");
        assert_eq!(count(&edges, &RelationType::RespondedTo), 0);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Rule 2: TriggeredBy
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn triggered_by_tool_call_links_nearest_preceding_agent_step() {
        let entities = vec![
            make_entity(EntityType::AgentStep, 0),
            make_entity(EntityType::AgentStep, 1),
            make_entity(EntityType::ToolCallEvent, 2),
        ];
        let edges = extract(&entities, "h");
        let tb: Vec<_> = edges
            .iter()
            .filter(|e| e.relation_type == RelationType::TriggeredBy)
            .collect();
        assert_eq!(tb.len(), 1);
        // nearest preceding agent step is eid-1
        assert_eq!(tb[0].source_entity_id, "eid-2");
        assert_eq!(tb[0].target_entity_id, "eid-1");
        assert!((tb[0].confidence - 0.7).abs() < f32::EPSILON);
    }

    #[test]
    fn triggered_by_no_preceding_agent_step_no_edge() {
        let entities = vec![make_entity(EntityType::ToolCallEvent, 0)];
        let edges = extract(&entities, "h");
        assert_eq!(count(&edges, &RelationType::TriggeredBy), 0);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Rule 3: Generated
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn generated_explicit_match_on_tool_name() {
        let entities = vec![
            make_entity_with_tool(EntityType::ToolCallEvent, 0, Some("web_search")),
            make_entity_with_tool(EntityType::ToolResultEvent, 1, Some("web_search")),
        ];
        let edges = extract(&entities, "h");
        let gen: Vec<_> = edges
            .iter()
            .filter(|e| e.relation_type == RelationType::Generated)
            .collect();
        assert_eq!(gen.len(), 1);
        assert_eq!(gen[0].source_entity_id, "eid-0");
        assert_eq!(gen[0].target_entity_id, "eid-1");
        assert!((gen[0].confidence - 1.0).abs() < f32::EPSILON);
        assert_eq!(gen[0].source, RelationSource::Explicit);
    }

    #[test]
    fn generated_fallback_positional_when_no_tool_name() {
        let entities = vec![
            make_entity(EntityType::ToolCallEvent, 0),
            make_entity(EntityType::PromptEvent, 1),
            make_entity(EntityType::ToolResultEvent, 2),
        ];
        let edges = extract(&entities, "h");
        let gen: Vec<_> = edges
            .iter()
            .filter(|e| e.relation_type == RelationType::Generated)
            .collect();
        assert_eq!(gen.len(), 1);
        assert_eq!(gen[0].source_entity_id, "eid-0");
        assert_eq!(gen[0].target_entity_id, "eid-2");
        assert!((gen[0].confidence - 0.7).abs() < f32::EPSILON);
        assert_eq!(gen[0].source, RelationSource::Inferred);
    }

    #[test]
    fn generated_fallback_when_tool_name_mismatch() {
        // call has tool_name "search", result has "calculator" — no explicit match
        let entities = vec![
            make_entity_with_tool(EntityType::ToolCallEvent, 0, Some("search")),
            make_entity_with_tool(EntityType::ToolResultEvent, 1, Some("calculator")),
        ];
        let edges = extract(&entities, "h");
        let gen: Vec<_> = edges
            .iter()
            .filter(|e| e.relation_type == RelationType::Generated)
            .collect();
        assert_eq!(gen.len(), 1);
        // Falls back to positional nearest
        assert!((gen[0].confidence - 0.7).abs() < f32::EPSILON);
        assert_eq!(gen[0].source, RelationSource::Inferred);
    }

    #[test]
    fn generated_no_result_no_edge() {
        let entities = vec![make_entity(EntityType::ToolCallEvent, 0)];
        let edges = extract(&entities, "h");
        assert_eq!(count(&edges, &RelationType::Generated), 0);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Rule 4: Informed
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn informed_retrieval_links_nearest_subsequent_prompt() {
        let entities = vec![
            make_entity(EntityType::RetrievalEvent, 0),
            make_entity(EntityType::CompletionEvent, 1), // not a prompt
            make_entity(EntityType::PromptEvent, 2),
            make_entity(EntityType::PromptEvent, 3),
        ];
        let edges = extract(&entities, "h");
        let inf: Vec<_> = edges
            .iter()
            .filter(|e| e.relation_type == RelationType::Informed)
            .collect();
        assert_eq!(inf.len(), 1);
        // nearest subsequent PromptEvent is eid-2
        assert_eq!(inf[0].source_entity_id, "eid-0");
        assert_eq!(inf[0].target_entity_id, "eid-2");
        assert!((inf[0].confidence - 0.7).abs() < f32::EPSILON);
    }

    #[test]
    fn informed_no_subsequent_prompt_no_edge() {
        let entities = vec![
            make_entity(EntityType::RetrievalEvent, 0),
            make_entity(EntityType::CompletionEvent, 1),
        ];
        let edges = extract(&entities, "h");
        assert_eq!(count(&edges, &RelationType::Informed), 0);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Rule 6: DelegatedTo
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn delegated_to_agent_step_links_nearest_mcp() {
        let entities = vec![
            make_entity(EntityType::AgentStep, 0),
            make_entity(EntityType::ToolCallEvent, 1),
            make_entity(EntityType::McpEvent, 2),
            make_entity(EntityType::McpEvent, 3),
        ];
        let edges = extract(&entities, "h");
        let dt: Vec<_> = edges
            .iter()
            .filter(|e| e.relation_type == RelationType::DelegatedTo)
            .collect();
        assert_eq!(dt.len(), 1);
        // nearest subsequent McpEvent is eid-2
        assert_eq!(dt[0].source_entity_id, "eid-0");
        assert_eq!(dt[0].target_entity_id, "eid-2");
        assert!((dt[0].confidence - 0.7).abs() < f32::EPSILON);
    }

    #[test]
    fn delegated_to_no_mcp_no_edge() {
        let entities = vec![make_entity(EntityType::AgentStep, 0)];
        let edges = extract(&entities, "h");
        assert_eq!(count(&edges, &RelationType::DelegatedTo), 0);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Edge uniqueness and metadata
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn all_edges_have_unique_relation_ids() {
        let entities = vec![
            make_entity(EntityType::PromptEvent, 0),
            make_entity(EntityType::RetrievalEvent, 1),
            make_entity(EntityType::ContextWindow, 2),
            make_entity(EntityType::AgentStep, 3),
            make_entity_with_tool(EntityType::ToolCallEvent, 4, Some("search")),
            make_entity_with_tool(EntityType::ToolResultEvent, 5, Some("search")),
            make_entity(EntityType::CompletionEvent, 6),
        ];
        let edges = extract(&entities, "myHash");
        let ids: std::collections::HashSet<&str> =
            edges.iter().map(|e| e.relation_id.as_str()).collect();
        assert_eq!(ids.len(), edges.len(), "all relation_ids are unique");
    }

    #[test]
    fn all_edges_carry_sample_hash() {
        let entities = vec![
            make_entity(EntityType::PromptEvent, 0),
            make_entity(EntityType::CompletionEvent, 1),
        ];
        let edges = extract(&entities, "myhash42");
        assert!(
            edges.iter().all(|e| e.sample_hash == "myhash42"),
            "every edge carries the sample_hash"
        );
    }

    #[test]
    fn relation_id_is_content_derived_hex() {
        // IDs are now SHA-256(sample_hash, type, src, dst) truncated to
        // 32 hex chars — no dashes, all lowercase hex.
        let entities = vec![make_entity(EntityType::PromptEvent, 0)];
        let edges = extract(&entities, "h");
        for edge in &edges {
            assert_eq!(edge.relation_id.len(), 32, "32 hex chars");
            assert!(
                edge.relation_id.chars().all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
                "lowercase hex only: {}",
                edge.relation_id,
            );
        }
    }

    #[test]
    fn relation_id_is_deterministic() {
        // Re-running extract() against identical inputs must produce the same
        // relation_ids — this is the property that makes upserts in
        // `output::graph::write_edges` actually idempotent.
        let entities = vec![
            make_entity(EntityType::PromptEvent, 0),
            make_entity(EntityType::CompletionEvent, 1),
        ];
        let a = extract(&entities, "h");
        let b = extract(&entities, "h");
        assert_eq!(a.len(), b.len());
        let ids_a: Vec<&str> = a.iter().map(|e| e.relation_id.as_str()).collect();
        let ids_b: Vec<&str> = b.iter().map(|e| e.relation_id.as_str()).collect();
        assert_eq!(ids_a, ids_b, "relation_ids must be stable across runs");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Integration tests — fixture files
    // ─────────────────────────────────────────────────────────────────────────

    /// Build a minimal entity list directly from a fixture's expected shape
    /// rather than running the full pipeline, so these tests are fast and
    /// deterministic.

    // openai_chat_completions.log entity shape:
    //   line 0: PromptEvent (system)
    //   line 1: PromptEvent (user)
    //   line 2: ToolCallEvent (web_search)
    //   line 3: RetrievalEvent (embedding_lookup)
    //   line 4: ToolResultEvent (web_search)
    //   line 5: ContextWindow
    //   line 6: CompletionEvent
    fn openai_entities() -> Vec<EntityRecord> {
        vec![
            make_entity(EntityType::PromptEvent, 0),
            make_entity(EntityType::PromptEvent, 1),
            make_entity_with_tool(EntityType::ToolCallEvent, 2, Some("web_search")),
            make_entity(EntityType::RetrievalEvent, 3),
            make_entity_with_tool(EntityType::ToolResultEvent, 4, Some("web_search")),
            make_entity(EntityType::ContextWindow, 5),
            make_entity(EntityType::CompletionEvent, 6),
        ]
    }

    #[test]
    fn openai_fixture_relation_counts() {
        let entities = openai_entities();
        let edges = extract(&entities, "openai_hash");

        // Rule 8: 7 PartOf edges
        assert_eq!(count(&edges, &RelationType::PartOf), 7);

        // Rule 7: CW (line 5) assembled from lines 0-4 → 5 AssembledFrom
        assert_eq!(count(&edges, &RelationType::AssembledFrom), 5);

        // Rule 1: 1 CompletionEvent ← 1 RespondedTo (nearest PromptEvent = line 1)
        assert_eq!(count(&edges, &RelationType::RespondedTo), 1);

        // Rule 3: 1 ToolCallEvent + 1 ToolResultEvent with matching tool_name → Explicit
        let gen: Vec<_> = edges
            .iter()
            .filter(|e| e.relation_type == RelationType::Generated)
            .collect();
        assert_eq!(gen.len(), 1);
        assert!((gen[0].confidence - 1.0).abs() < f32::EPSILON);
        assert_eq!(gen[0].source, RelationSource::Explicit);

        // Rule 4: RetrievalEvent (3) → nearest subsequent PromptEvent — none after 3; no Informed
        // (PromptEvents are at 0 and 1, both before index 3)
        assert_eq!(count(&edges, &RelationType::Informed), 0);
    }

    // react_agent.log entity shape:
    //   AgentStep(0), AgentStep(1), AgentStep(2), ToolCallEvent(3),
    //   AgentStep(4), AgentStep(5), ToolCallEvent(6), AgentStep(7)
    fn react_entities() -> Vec<EntityRecord> {
        vec![
            make_entity(EntityType::AgentStep, 0), // AgentExecutor init
            make_entity(EntityType::AgentStep, 1), // Thought 1
            make_entity(EntityType::AgentStep, 2), // Action web_search line
            make_entity_with_tool(EntityType::ToolCallEvent, 3, Some("web_search")),
            make_entity(EntityType::AgentStep, 4), // Observation → actually ToolResultEvent, but
            // in the ReAct fixture the Observation: lines
            // were also ToolResultEvent — let's use that
            make_entity_with_tool(EntityType::ToolResultEvent, 5, Some("web_search")),
            make_entity(EntityType::AgentStep, 6), // Thought 2
            make_entity_with_tool(EntityType::ToolCallEvent, 7, Some("calculator")),
            make_entity_with_tool(EntityType::ToolResultEvent, 8, Some("calculator")),
            make_entity(EntityType::AgentStep, 9), // Final Answer
        ]
    }

    #[test]
    fn react_fixture_has_followed_by_chain() {
        let entities = react_entities();
        let edges = extract(&entities, "react_hash");

        // AgentSteps in react_entities: lines 0,1,2,4,6,9 → 5 FollowedBy pairs
        let fb = count(&edges, &RelationType::FollowedBy);
        assert!(fb >= 3, "at least 3 FollowedBy edges in a ReAct loop, got {fb}");
    }

    #[test]
    fn react_fixture_tool_calls_triggered_by_agent_steps() {
        let entities = react_entities();
        let edges = extract(&entities, "react_hash");

        let tb = count(&edges, &RelationType::TriggeredBy);
        assert!(tb >= 1, "at least 1 TriggeredBy for ToolCallEvent after AgentStep");
    }

    #[test]
    fn react_fixture_generated_explicit_for_matching_tool_names() {
        let entities = react_entities();
        let edges = extract(&entities, "react_hash");

        let explicit_gen: Vec<_> = edges
            .iter()
            .filter(|e| {
                e.relation_type == RelationType::Generated && e.source == RelationSource::Explicit
            })
            .collect();
        assert!(
            !explicit_gen.is_empty(),
            "explicit Generated edges expected when tool_names match"
        );
    }

    // mcp_jsonrpc.log entity shape:
    //   McpEvent × 5 pairs (all McpEvent, no AgentSteps in this fixture)
    fn mcp_entities() -> Vec<EntityRecord> {
        (0..10u32).map(|i| make_entity(EntityType::McpEvent, i)).collect()
    }

    #[test]
    fn mcp_fixture_only_part_of_edges() {
        let entities = mcp_entities();
        let edges = extract(&entities, "mcp_hash");

        // Only PartOf edges expected — no AgentSteps, no ToolCall/Results, no Prompts
        assert_eq!(count(&edges, &RelationType::PartOf), 10);
        assert_eq!(count(&edges, &RelationType::FollowedBy), 0);
        assert_eq!(count(&edges, &RelationType::TriggeredBy), 0);
        assert_eq!(count(&edges, &RelationType::Generated), 0);
        assert_eq!(count(&edges, &RelationType::RespondedTo), 0);
        assert_eq!(count(&edges, &RelationType::AssembledFrom), 0);
        assert_eq!(count(&edges, &RelationType::Informed), 0);
        assert_eq!(count(&edges, &RelationType::DelegatedTo), 0);
    }

    #[test]
    fn extract_empty_entities_returns_empty() {
        let edges = extract(&[], "h");
        assert!(edges.is_empty());
    }

    #[test]
    fn extract_is_idempotent_same_counts() {
        let entities = openai_entities();
        let edges1 = extract(&entities, "h");
        let edges2 = extract(&entities, "h");
        // IDs are now content-derived, so re-running produces identical
        // edges (same count AND same relation_ids).  The counts-only
        // assertion is retained as a regression guard.
        assert_eq!(edges1.len(), edges2.len());
        assert_eq!(
            count(&edges1, &RelationType::PartOf),
            count(&edges2, &RelationType::PartOf)
        );
        assert_eq!(
            count(&edges1, &RelationType::Generated),
            count(&edges2, &RelationType::Generated)
        );
        // Stronger assertion: full ID equality across runs.
        let ids1: Vec<&str> = edges1.iter().map(|e| e.relation_id.as_str()).collect();
        let ids2: Vec<&str> = edges2.iter().map(|e| e.relation_id.as_str()).collect();
        assert_eq!(ids1, ids2, "relation_ids must be stable across runs");
    }
}
