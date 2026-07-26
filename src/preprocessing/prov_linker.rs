//! Stage 9 — emit W3C PROV-O provenance triples from entity and relation data.
//!
//! Produces [`ProvTriple`] records that conform to the [W3C PROV-O] ontology.
//! Triples are stored in the `prov_relations` MongoDB collection by the Phase 6
//! graph output adapter.
//!
//! [W3C PROV-O]: https://www.w3.org/TR/prov-o/
//!
//! # URI scheme
//! | Node kind | URI pattern |
//! |-----------|-------------|
//! | Entity    | `ug:entity:{entity_id}` |
//! | Activity  | `ug:activity:{sample_hash}:{line_index}` |
//! | Agent     | `ug:agent:{model_id}` or `ug:agent:{target_id}` |
//!
//! # Predicates emitted
//! | Predicate | Condition |
//! |-----------|-----------|
//! | `prov:wasGeneratedBy` | every entity → its activity (log line) |
//! | `prov:wasAttributedTo` | every entity → its agent (model or target) |
//! | `prov:wasDerivedFrom` | for data-lineage [`RelationEdge`] types: `Generated`, `Informed`, `RespondedTo`, `AssembledFrom` |

use mongodb::bson::DateTime;
use serde::{Deserialize, Serialize};

use crate::models::{EntityRecord, RelationEdge, RelationType};

// ─── PROV types ───────────────────────────────────────────────────────────────

/// PROV-O predicate (a subset relevant to UpsideGate).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProvPredicate {
    /// `prov:wasGeneratedBy` — entity was produced by an activity (log line).
    WasGeneratedBy,
    /// `prov:wasAttributedTo` — entity is attributed to an agent (LLM model or host).
    WasAttributedTo,
    /// `prov:wasDerivedFrom` — entity data lineage from another entity.
    WasDerivedFrom,
    /// `prov:used` — an activity consumed this entity as input.
    Used,
    /// `prov:actedOnBehalfOf` — sub-agent delegated through a super-agent.
    ActedOnBehalfOf,
}

/// A single W3C PROV-O triple: `(subject, predicate, object)`.
///
/// All three fields are URI strings.  Written to the `prov_relations`
/// collection by the Phase 6 graph output adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvTriple {
    /// Subject URI (`ug:entity:…`, `ug:activity:…`, or `ug:agent:…`).
    pub subject: String,
    pub predicate: ProvPredicate,
    /// Object URI.
    pub object: String,
    /// FK to the parent sample — enables efficient range queries.
    pub sample_hash: String,
    pub created_at: DateTime,
}

// ─── URI builders ─────────────────────────────────────────────────────────────

/// Construct an agent URI from a model identifier or, as fallback, a target ID.
fn agent_uri(model_id: Option<&str>, target_id: &str) -> String {
    match model_id {
        Some(m) if !m.is_empty() => format!("ug:agent:{m}"),
        _ => format!("ug:agent:{target_id}"),
    }
}

fn triple(
    subject: &str,
    predicate: ProvPredicate,
    object: &str,
    sample_hash: &str,
) -> ProvTriple {
    ProvTriple {
        subject: subject.to_string(),
        predicate,
        object: object.to_string(),
        sample_hash: sample_hash.to_string(),
        created_at: DateTime::now(),
    }
}

// ─── Public entry point ───────────────────────────────────────────────────────

/// Build PROV-O triples from the typed entity list and relation edges.
///
/// Entities are expected to carry pre-populated `prov_entity_id` and
/// `prov_activity_id` fields (set by [`super::entity_extractor`]).
///
/// # Arguments
/// * `entities`    – typed, classified entities from Stages 6–7
/// * `relations`   – directed edges from Stage 8
/// * `sample_hash` – stable hash of the parent [`crate::models::SampleRecord`]
pub fn build(
    entities: &[EntityRecord],
    relations: &[RelationEdge],
    sample_hash: &str,
) -> Vec<ProvTriple> {
    let mut triples: Vec<ProvTriple> = Vec::new();

    // ── Per-entity triples ────────────────────────────────────────────────────

    for e in entities {
        // prov:wasGeneratedBy — the entity was produced by the activity at this
        // log line.
        triples.push(triple(
            &e.prov_entity_id,
            ProvPredicate::WasGeneratedBy,
            &e.prov_activity_id,
            sample_hash,
        ));

        // prov:wasAttributedTo — the entity is attributed to the LLM model that
        // produced it, or to the target host if no model is identifiable.
        let agent = agent_uri(e.model_id.as_deref(), &e.target_id);
        triples.push(triple(
            &e.prov_entity_id,
            ProvPredicate::WasAttributedTo,
            &agent,
            sample_hash,
        ));
    }

    // ── Per-relation triples (data-lineage edges only) ────────────────────────

    // Map RelationEdge types that represent data lineage to prov:wasDerivedFrom.
    // Causal / temporal edges (TriggeredBy, FollowedBy, DelegatedTo, PartOf)
    // do not imply data lineage and are omitted.
    for r in relations {
        let emit = matches!(
            r.relation_type,
            RelationType::Generated
                | RelationType::Informed
                | RelationType::RespondedTo
                | RelationType::AssembledFrom
        );
        if !emit {
            continue;
        }
        triples.push(triple(
            &format!("ug:entity:{}", r.source_entity_id),
            ProvPredicate::WasDerivedFrom,
            &format!("ug:entity:{}", r.target_entity_id),
            sample_hash,
        ));
    }

    triples
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use mongodb::bson::DateTime;
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::models::{
        EntityRecord, EntityType, RelationEdge, RelationSource, RelationType, SemanticRole,
    };

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn make_entity(
        entity_id: &str,
        entity_type: EntityType,
        line_index: u32,
        model_id: Option<&str>,
    ) -> EntityRecord {
        EntityRecord {
            entity_id: entity_id.to_string(),
            entity_type,
            semantic_role: SemanticRole::Unknown,
            sample_hash: "sh".to_string(),
            target_id: "target-001".to_string(),
            trace_id: "trace001".to_string(),
            span_id: format!("span{line_index:04}"),
            parent_span_id: None,
            prov_entity_id: format!("ug:entity:{entity_id}"),
            prov_activity_id: format!("ug:activity:sh:{line_index}"),
            line_index,
            raw_text: String::new(),
            extracted_fields: HashMap::new(),
            model_id: model_id.map(str::to_string),
            tool_name: None,
            mcp_server_id: None,
            token_count: None,
            latency_ms: None,
            timestamp_utc: None,
            content_embedding_id: None,
            behavioral_embedding_id: None,
            task_id: String::new(),
            correlation_key: None,
        }
    }

    fn make_relation(
        relation_type: RelationType,
        src: &str,
        tgt: &str,
    ) -> RelationEdge {
        RelationEdge {
            relation_id: format!("rid-{src}-{tgt}"),
            relation_type,
            source_entity_id: src.to_string(),
            target_entity_id: tgt.to_string(),
            sample_hash: "sh".to_string(),
            confidence: 0.7,
            source: RelationSource::Inferred,
            created_at: DateTime::now(),
        }
    }

    fn count_pred(triples: &[ProvTriple], pred: &ProvPredicate) -> usize {
        triples.iter().filter(|t| &t.predicate == pred).count()
    }

    // ─────────────────────────────────────────────────────────────────────────
    // wasGeneratedBy
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn was_generated_by_emitted_for_every_entity() {
        let entities = vec![
            make_entity("e0", EntityType::PromptEvent, 0, None),
            make_entity("e1", EntityType::CompletionEvent, 1, Some("gpt-4o")),
        ];
        let triples = build(&entities, &[], "sh");
        assert_eq!(count_pred(&triples, &ProvPredicate::WasGeneratedBy), 2);
    }

    #[test]
    fn was_generated_by_uses_prov_activity_id() {
        let entities = vec![make_entity("e0", EntityType::PromptEvent, 5, None)];
        let triples = build(&entities, &[], "sh");
        let t = triples
            .iter()
            .find(|t| t.predicate == ProvPredicate::WasGeneratedBy)
            .unwrap();
        assert_eq!(t.subject, "ug:entity:e0");
        assert_eq!(t.object, "ug:activity:sh:5");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // wasAttributedTo
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn was_attributed_to_emitted_for_every_entity() {
        let entities = vec![
            make_entity("e0", EntityType::PromptEvent, 0, None),
            make_entity("e1", EntityType::CompletionEvent, 1, Some("gpt-4o")),
        ];
        let triples = build(&entities, &[], "sh");
        assert_eq!(count_pred(&triples, &ProvPredicate::WasAttributedTo), 2);
    }

    #[test]
    fn was_attributed_to_uses_model_id_when_present() {
        let entities = vec![make_entity("e0", EntityType::CompletionEvent, 0, Some("gpt-4o"))];
        let triples = build(&entities, &[], "sh");
        let t = triples
            .iter()
            .find(|t| t.predicate == ProvPredicate::WasAttributedTo)
            .unwrap();
        assert_eq!(t.object, "ug:agent:gpt-4o");
    }

    #[test]
    fn was_attributed_to_falls_back_to_target_id() {
        let entities = vec![make_entity("e0", EntityType::PromptEvent, 0, None)];
        let triples = build(&entities, &[], "sh");
        let t = triples
            .iter()
            .find(|t| t.predicate == ProvPredicate::WasAttributedTo)
            .unwrap();
        // No model_id → falls back to ug:agent:{target_id}
        assert_eq!(t.object, "ug:agent:target-001");
    }

    #[test]
    fn was_attributed_to_empty_model_id_falls_back() {
        // Empty string model_id should behave like None
        let mut e = make_entity("e0", EntityType::PromptEvent, 0, Some(""));
        e.model_id = Some(String::new()); // explicitly empty
        let triples = build(&[e], &[], "sh");
        let t = triples
            .iter()
            .find(|t| t.predicate == ProvPredicate::WasAttributedTo)
            .unwrap();
        assert_eq!(t.object, "ug:agent:target-001");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // wasDerivedFrom (relation-mapped)
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn was_derived_from_emitted_for_generated_edge() {
        let entities = vec![make_entity("e0", EntityType::ToolCallEvent, 0, None)];
        let relations = vec![make_relation(RelationType::Generated, "e0", "e1")];
        let triples = build(&entities, &relations, "sh");
        let wdf: Vec<_> = triples
            .iter()
            .filter(|t| t.predicate == ProvPredicate::WasDerivedFrom)
            .collect();
        assert_eq!(wdf.len(), 1);
        assert_eq!(wdf[0].subject, "ug:entity:e0");
        assert_eq!(wdf[0].object, "ug:entity:e1");
    }

    #[test]
    fn was_derived_from_emitted_for_informed_edge() {
        let entities = vec![make_entity("e0", EntityType::RetrievalEvent, 0, None)];
        let relations = vec![make_relation(RelationType::Informed, "e0", "e1")];
        let triples = build(&entities, &relations, "sh");
        assert_eq!(count_pred(&triples, &ProvPredicate::WasDerivedFrom), 1);
    }

    #[test]
    fn was_derived_from_emitted_for_responded_to_edge() {
        let entities = vec![make_entity("e0", EntityType::CompletionEvent, 0, None)];
        let relations = vec![make_relation(RelationType::RespondedTo, "e0", "e1")];
        let triples = build(&entities, &relations, "sh");
        assert_eq!(count_pred(&triples, &ProvPredicate::WasDerivedFrom), 1);
    }

    #[test]
    fn was_derived_from_emitted_for_assembled_from_edge() {
        let entities = vec![make_entity("e0", EntityType::ContextWindow, 0, None)];
        let relations = vec![make_relation(RelationType::AssembledFrom, "e0", "e1")];
        let triples = build(&entities, &relations, "sh");
        assert_eq!(count_pred(&triples, &ProvPredicate::WasDerivedFrom), 1);
    }

    #[test]
    fn causal_edges_do_not_produce_was_derived_from() {
        let causal_types = [
            RelationType::TriggeredBy,
            RelationType::FollowedBy,
            RelationType::DelegatedTo,
            RelationType::PartOf,
        ];
        for rt in &causal_types {
            let relations = vec![make_relation(rt.clone(), "e0", "e1")];
            let triples = build(&[], &relations, "sh");
            assert_eq!(
                count_pred(&triples, &ProvPredicate::WasDerivedFrom),
                0,
                "{rt:?} should not produce wasDerivedFrom"
            );
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Counts and sample_hash propagation
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn total_triples_per_entity_is_two_without_relations() {
        // Each entity → wasGeneratedBy + wasAttributedTo = 2 triples
        let entities = vec![
            make_entity("e0", EntityType::PromptEvent, 0, None),
            make_entity("e1", EntityType::AgentStep, 1, Some("claude-3")),
            make_entity("e2", EntityType::ToolCallEvent, 2, None),
        ];
        let triples = build(&entities, &[], "sh");
        assert_eq!(triples.len(), 6, "2 triples per entity");
    }

    #[test]
    fn all_triples_carry_sample_hash() {
        let entities = vec![make_entity("e0", EntityType::PromptEvent, 0, None)];
        let relations = vec![make_relation(RelationType::Generated, "e0", "e1")];
        let triples = build(&entities, &relations, "myhash");
        assert!(
            triples.iter().all(|t| t.sample_hash == "myhash"),
            "every triple carries the sample_hash"
        );
    }

    #[test]
    fn empty_input_returns_empty() {
        let triples = build(&[], &[], "sh");
        assert!(triples.is_empty());
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Integration-style: mixed entity set + relations
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn openai_shape_triple_counts() {
        // 7 entities (from openai_chat_completions fixture shape)
        let entities = vec![
            make_entity("e0", EntityType::PromptEvent, 0, None),
            make_entity("e1", EntityType::PromptEvent, 1, None),
            make_entity("e2", EntityType::ToolCallEvent, 2, None),
            make_entity("e3", EntityType::RetrievalEvent, 3, None),
            make_entity("e4", EntityType::ToolResultEvent, 4, None),
            make_entity("e5", EntityType::ContextWindow, 5, None),
            make_entity("e6", EntityType::CompletionEvent, 6, Some("gpt-4o")),
        ];
        // Include one Generated and one RespondedTo edge
        let relations = vec![
            make_relation(RelationType::Generated, "e2", "e4"),
            make_relation(RelationType::RespondedTo, "e6", "e1"),
            make_relation(RelationType::PartOf, "e0", "traceid"), // should NOT produce wasDerivedFrom
        ];
        let triples = build(&entities, &relations, "oh");

        // 7 entities × 2 = 14 entity triples + 2 wasDerivedFrom (Generated + RespondedTo)
        assert_eq!(triples.len(), 16);
        assert_eq!(count_pred(&triples, &ProvPredicate::WasGeneratedBy), 7);
        assert_eq!(count_pred(&triples, &ProvPredicate::WasAttributedTo), 7);
        assert_eq!(count_pred(&triples, &ProvPredicate::WasDerivedFrom), 2);

        // gpt-4o entity should be attributed to the model
        let gpt_attr = triples
            .iter()
            .find(|t| t.predicate == ProvPredicate::WasAttributedTo && t.object == "ug:agent:gpt-4o")
            .unwrap();
        assert_eq!(gpt_attr.subject, "ug:entity:e6");
    }
}
