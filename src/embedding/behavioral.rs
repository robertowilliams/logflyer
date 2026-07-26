//! Behavioral embedding — a fixed-size feature vector derived from entity and
//! relation statistics with no external API calls.
//!
//! The vector captures the *structural* fingerprint of a log sample: which
//! entity types appear, how they are distributed across semantic roles, how
//! dense the relation graph is, and how strong the agentic signal is.
//!
//! # Vector layout (36 dimensions)
//!
//! | Dims   | Feature group | Normalisation |
//! |--------|---------------|---------------|
//! | 0 – 8  | Entity-type histogram (9 types) | count / n_entities |
//! | 9 – 24 | Semantic-role histogram (16 roles) | count / n_entities |
//! | 25 – 32| Relation-type histogram (8 types) | count / n_relations |
//! | 33     | Entity density | min(n_entities / 50, 1) |
//! | 34     | Relation density | min(n_relations / 200, 1) |
//! | 35     | Agentic signal score | as-is (already ≈ [0, 1]) |

use crate::models::{EntityRecord, EntityType, RelationEdge, RelationType, SemanticRole};

// ─── Dimension constant ───────────────────────────────────────────────────────

/// Total number of dimensions in the behavioral vector.
pub const BEHAVIORAL_DIM: usize = 36;

/// Model identifier stored in [`super::EmbeddingRecord`] for behavioral vectors.
pub const BEHAVIORAL_MODEL: &str = "behavioral-v1";

// ─── Fixed orderings ──────────────────────────────────────────────────────────

/// Canonical order of [`EntityType`] variants (dims 0–8).
///
/// `static` rather than `const` because `EntityType` does not derive `Copy`.
static ENTITY_TYPES: [EntityType; 9] = [
    EntityType::PromptEvent,
    EntityType::CompletionEvent,
    EntityType::ToolCallEvent,
    EntityType::ToolResultEvent,
    EntityType::RetrievalEvent,
    EntityType::AgentStep,
    EntityType::McpEvent,
    EntityType::ContextWindow,
    EntityType::Unknown,
];

/// Canonical order of [`SemanticRole`] variants (dims 9–24).
static SEMANTIC_ROLES: [SemanticRole; 16] = [
    SemanticRole::SystemPrompt,
    SemanticRole::UserTurn,
    SemanticRole::AssistantTurn,
    SemanticRole::ToolInvocation,
    SemanticRole::ToolResponse,
    SemanticRole::RetrievalQuery,
    SemanticRole::RetrievalResult,
    SemanticRole::AgentReasoning,
    SemanticRole::AgentAction,
    SemanticRole::AgentObservation,
    SemanticRole::McpRequest,
    SemanticRole::McpResponse,
    SemanticRole::ContextAssembly,
    SemanticRole::MemoryRead,
    SemanticRole::MemoryWrite,
    SemanticRole::Unknown,
];

/// Canonical order of [`RelationType`] variants (dims 25–32).
static RELATION_TYPES: [RelationType; 8] = [
    RelationType::TriggeredBy,
    RelationType::Generated,
    RelationType::Informed,
    RelationType::FollowedBy,
    RelationType::RespondedTo,
    RelationType::AssembledFrom,
    RelationType::PartOf,
    RelationType::DelegatedTo,
];

// Scalar feature caps for dims 33–34.
const ENTITY_DENSITY_CAP: f32 = 50.0;
const RELATION_DENSITY_CAP: f32 = 200.0;

// ─── Public entry point ───────────────────────────────────────────────────────

/// Compute a [`BEHAVIORAL_DIM`]-dimensional feature vector from entity and
/// relation data.
///
/// The computation is pure and deterministic — same inputs always produce the
/// same vector.  No I/O is performed.
///
/// # Arguments
/// * `entities`     – typed, classified entities from Stages 6–7
/// * `relations`    – directed edges from Stage 8
/// * `signal_score` – agentic signal score from [`AgenticScan`] (dim 35)
pub fn compute(
    entities: &[EntityRecord],
    relations: &[RelationEdge],
    signal_score: f64,
) -> Vec<f32> {
    let mut v = vec![0.0f32; BEHAVIORAL_DIM];
    let n_entities = entities.len() as f32;
    let n_relations = relations.len() as f32;

    // ── Dims 0–8: entity-type histogram ──────────────────────────────────────
    for e in entities {
        if let Some(i) = ENTITY_TYPES.iter().position(|t| t == &e.entity_type) {
            v[i] += 1.0;
        }
    }
    if n_entities > 0.0 {
        for i in 0..ENTITY_TYPES.len() {
            v[i] /= n_entities;
        }
    }

    // ── Dims 9–24: semantic-role histogram ────────────────────────────────────
    let role_base = ENTITY_TYPES.len(); // 9
    for e in entities {
        if let Some(i) = SEMANTIC_ROLES.iter().position(|r| r == &e.semantic_role) {
            v[role_base + i] += 1.0;
        }
    }
    if n_entities > 0.0 {
        for i in 0..SEMANTIC_ROLES.len() {
            v[role_base + i] /= n_entities;
        }
    }

    // ── Dims 25–32: relation-type histogram ───────────────────────────────────
    let rel_base = role_base + SEMANTIC_ROLES.len(); // 25
    for r in relations {
        if let Some(i) = RELATION_TYPES.iter().position(|t| t == &r.relation_type) {
            v[rel_base + i] += 1.0;
        }
    }
    if n_relations > 0.0 {
        for i in 0..RELATION_TYPES.len() {
            v[rel_base + i] /= n_relations;
        }
    }

    // ── Dim 33: entity density ────────────────────────────────────────────────
    v[33] = (n_entities / ENTITY_DENSITY_CAP).min(1.0);

    // ── Dim 34: relation density ──────────────────────────────────────────────
    v[34] = (n_relations / RELATION_DENSITY_CAP).min(1.0);

    // ── Dim 35: agentic signal score ──────────────────────────────────────────
    v[35] = (signal_score as f32).clamp(0.0, 1.0);

    v
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use mongodb::bson::DateTime;
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::models::{EntityRecord, EntityType, RelationEdge, RelationSource, RelationType,
                        SemanticRole};

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn entity(entity_type: EntityType, role: SemanticRole) -> EntityRecord {
        EntityRecord {
            entity_id: "e".to_string(),
            entity_type,
            semantic_role: role,
            sample_hash: "sh".to_string(),
            target_id: "t".to_string(),
            trace_id: "trace".to_string(),
            span_id: "span".to_string(),
            parent_span_id: None,
            prov_entity_id: "ug:entity:e".to_string(),
            prov_activity_id: "ug:activity:sh:0".to_string(),
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
        }
    }

    fn relation(relation_type: RelationType) -> RelationEdge {
        RelationEdge {
            relation_id: "r".to_string(),
            relation_type,
            source_entity_id: "s".to_string(),
            target_entity_id: "t".to_string(),
            sample_hash: "sh".to_string(),
            confidence: 0.7,
            source: RelationSource::Inferred,
            created_at: DateTime::now(),
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Dimensionality
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn vector_has_behavioral_dim_elements() {
        let v = compute(&[], &[], 0.0);
        assert_eq!(v.len(), BEHAVIORAL_DIM);
        assert_eq!(BEHAVIORAL_DIM, 36);
    }

    #[test]
    fn empty_input_zero_vector_except_scalars() {
        let v = compute(&[], &[], 0.5);
        // Histogram dims all zero
        for &x in &v[..33] {
            assert_eq!(x, 0.0);
        }
        // Entity density = 0
        assert_eq!(v[33], 0.0);
        // Relation density = 0
        assert_eq!(v[34], 0.0);
        // Signal score
        assert!((v[35] - 0.5).abs() < 1e-6);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Entity-type histogram (dims 0–8)
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn single_prompt_event_sets_dim0_to_one() {
        let entities = vec![entity(EntityType::PromptEvent, SemanticRole::Unknown)];
        let v = compute(&entities, &[], 0.0);
        assert!((v[0] - 1.0).abs() < 1e-6, "dim 0 (PromptEvent) should be 1.0");
        for &x in &v[1..9] {
            assert_eq!(x, 0.0, "other entity-type dims should be 0");
        }
    }

    #[test]
    fn entity_type_histogram_normalised_to_one() {
        let entities = vec![
            entity(EntityType::PromptEvent, SemanticRole::Unknown),
            entity(EntityType::CompletionEvent, SemanticRole::Unknown),
            entity(EntityType::PromptEvent, SemanticRole::Unknown),
        ];
        let v = compute(&entities, &[], 0.0);
        // 2 PromptEvent out of 3 → dim 0 = 2/3
        assert!((v[0] - 2.0 / 3.0).abs() < 1e-6);
        // 1 CompletionEvent → dim 1 = 1/3
        assert!((v[1] - 1.0 / 3.0).abs() < 1e-6);
    }

    #[test]
    fn entity_type_histogram_sums_to_one_when_nonempty() {
        let entities = vec![
            entity(EntityType::ToolCallEvent, SemanticRole::Unknown),
            entity(EntityType::AgentStep, SemanticRole::Unknown),
            entity(EntityType::McpEvent, SemanticRole::Unknown),
        ];
        let v = compute(&entities, &[], 0.0);
        let sum: f32 = v[0..9].iter().sum();
        assert!((sum - 1.0).abs() < 1e-5, "entity type histogram sum = {sum}");
    }

    #[test]
    fn unknown_entity_type_maps_to_dim8() {
        let entities = vec![entity(EntityType::Unknown, SemanticRole::Unknown)];
        let v = compute(&entities, &[], 0.0);
        assert!((v[8] - 1.0).abs() < 1e-6);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Semantic-role histogram (dims 9–24)
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn system_prompt_role_maps_to_dim9() {
        let entities = vec![entity(EntityType::PromptEvent, SemanticRole::SystemPrompt)];
        let v = compute(&entities, &[], 0.0);
        assert!((v[9] - 1.0).abs() < 1e-6, "dim 9 (SystemPrompt) should be 1.0");
    }

    #[test]
    fn unknown_role_maps_to_dim24() {
        let entities = vec![entity(EntityType::AgentStep, SemanticRole::Unknown)];
        let v = compute(&entities, &[], 0.0);
        assert!((v[24] - 1.0).abs() < 1e-6, "dim 24 (Unknown role) should be 1.0");
    }

    #[test]
    fn semantic_role_histogram_sums_to_one_when_nonempty() {
        let entities = vec![
            entity(EntityType::PromptEvent, SemanticRole::SystemPrompt),
            entity(EntityType::PromptEvent, SemanticRole::UserTurn),
            entity(EntityType::CompletionEvent, SemanticRole::AssistantTurn),
        ];
        let v = compute(&entities, &[], 0.0);
        let sum: f32 = v[9..25].iter().sum();
        assert!((sum - 1.0).abs() < 1e-5, "semantic role histogram sum = {sum}");
    }

    #[test]
    fn memory_read_maps_to_dim22() {
        let entities = vec![entity(EntityType::Unknown, SemanticRole::MemoryRead)];
        let v = compute(&entities, &[], 0.0);
        assert!((v[22] - 1.0).abs() < 1e-6, "dim 22 (MemoryRead) should be 1.0");
    }

    #[test]
    fn memory_write_maps_to_dim23() {
        let entities = vec![entity(EntityType::Unknown, SemanticRole::MemoryWrite)];
        let v = compute(&entities, &[], 0.0);
        assert!((v[23] - 1.0).abs() < 1e-6, "dim 23 (MemoryWrite) should be 1.0");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Relation-type histogram (dims 25–32)
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn triggered_by_maps_to_dim25() {
        let relations = vec![relation(RelationType::TriggeredBy)];
        let v = compute(&[], &relations, 0.0);
        assert!((v[25] - 1.0).abs() < 1e-6, "dim 25 (TriggeredBy) should be 1.0");
    }

    #[test]
    fn part_of_maps_to_dim31() {
        let relations = vec![relation(RelationType::PartOf)];
        let v = compute(&[], &relations, 0.0);
        assert!((v[31] - 1.0).abs() < 1e-6, "dim 31 (PartOf) should be 1.0");
    }

    #[test]
    fn delegated_to_maps_to_dim32() {
        let relations = vec![relation(RelationType::DelegatedTo)];
        let v = compute(&[], &relations, 0.0);
        assert!((v[32] - 1.0).abs() < 1e-6, "dim 32 (DelegatedTo) should be 1.0");
    }

    #[test]
    fn relation_histogram_sums_to_one_when_nonempty() {
        let relations = vec![
            relation(RelationType::Generated),
            relation(RelationType::PartOf),
            relation(RelationType::PartOf),
        ];
        let v = compute(&[], &relations, 0.0);
        let sum: f32 = v[25..33].iter().sum();
        assert!((sum - 1.0).abs() < 1e-5, "relation type histogram sum = {sum}");
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Scalar features (dims 33–35)
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn entity_density_dim33_clamped_at_one() {
        // 50 entities → density = 1.0; 100 entities still → 1.0
        let fifty: Vec<EntityRecord> = (0..50)
            .map(|_| entity(EntityType::AgentStep, SemanticRole::Unknown))
            .collect();
        let v = compute(&fifty, &[], 0.0);
        assert!((v[33] - 1.0).abs() < 1e-6);

        let hundred: Vec<EntityRecord> = (0..100)
            .map(|_| entity(EntityType::AgentStep, SemanticRole::Unknown))
            .collect();
        let v2 = compute(&hundred, &[], 0.0);
        assert!((v2[33] - 1.0).abs() < 1e-6, "entity density capped at 1.0");
    }

    #[test]
    fn entity_density_dim33_proportional() {
        // 25 entities → density = 25/50 = 0.5
        let entities: Vec<EntityRecord> = (0..25)
            .map(|_| entity(EntityType::AgentStep, SemanticRole::Unknown))
            .collect();
        let v = compute(&entities, &[], 0.0);
        assert!((v[33] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn relation_density_dim34_proportional() {
        // 100 relations → density = 100/200 = 0.5
        let relations: Vec<RelationEdge> = (0..100)
            .map(|_| relation(RelationType::PartOf))
            .collect();
        let v = compute(&[], &relations, 0.0);
        assert!((v[34] - 0.5).abs() < 1e-5);
    }

    #[test]
    fn signal_score_dim35_carried_through() {
        let v = compute(&[], &[], 0.73);
        assert!((v[35] - 0.73f32).abs() < 1e-5);
    }

    #[test]
    fn signal_score_dim35_clamped_above_one() {
        let v = compute(&[], &[], 1.5);
        assert!((v[35] - 1.0).abs() < 1e-6, "signal score clamped to 1.0");
    }

    #[test]
    fn signal_score_dim35_clamped_below_zero() {
        let v = compute(&[], &[], -0.1);
        assert_eq!(v[35], 0.0);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Values are all in [0, 1]
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn all_values_in_unit_interval() {
        let entities = vec![
            entity(EntityType::PromptEvent, SemanticRole::SystemPrompt),
            entity(EntityType::CompletionEvent, SemanticRole::AssistantTurn),
            entity(EntityType::ToolCallEvent, SemanticRole::ToolInvocation),
            entity(EntityType::AgentStep, SemanticRole::AgentReasoning),
        ];
        let relations = vec![
            relation(RelationType::PartOf),
            relation(RelationType::Generated),
            relation(RelationType::FollowedBy),
        ];
        let v = compute(&entities, &relations, 0.42);
        for (i, &x) in v.iter().enumerate() {
            assert!(
                (0.0..=1.0).contains(&x),
                "dim {i} = {x} out of [0, 1]"
            );
        }
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Determinism
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn compute_is_deterministic() {
        let entities = vec![
            entity(EntityType::PromptEvent, SemanticRole::UserTurn),
            entity(EntityType::AgentStep, SemanticRole::AgentAction),
        ];
        let relations = vec![relation(RelationType::TriggeredBy)];
        let v1 = compute(&entities, &relations, 0.3);
        let v2 = compute(&entities, &relations, 0.3);
        assert_eq!(v1, v2);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Integration: openai-fixture entity shape
    // ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn openai_fixture_shape_produces_expected_dims() {
        // PromptEvent×2, ToolCallEvent×1, RetrievalEvent×1,
        // ToolResultEvent×1, ContextWindow×1, CompletionEvent×1 = 7 entities
        let entities = vec![
            entity(EntityType::PromptEvent, SemanticRole::SystemPrompt),
            entity(EntityType::PromptEvent, SemanticRole::UserTurn),
            entity(EntityType::ToolCallEvent, SemanticRole::ToolInvocation),
            entity(EntityType::RetrievalEvent, SemanticRole::RetrievalQuery),
            entity(EntityType::ToolResultEvent, SemanticRole::ToolResponse),
            entity(EntityType::ContextWindow, SemanticRole::ContextAssembly),
            entity(EntityType::CompletionEvent, SemanticRole::AssistantTurn),
        ];
        let v = compute(&entities, &[], 0.85);
        // PromptEvent dim (0) = 2/7
        assert!((v[0] - 2.0 / 7.0).abs() < 1e-5);
        // CompletionEvent dim (1) = 1/7
        assert!((v[1] - 1.0 / 7.0).abs() < 1e-5);
        // Entity density = 7/50
        assert!((v[33] - 7.0 / 50.0).abs() < 1e-5);
        // Signal score
        assert!((v[35] - 0.85f32).abs() < 1e-5);
    }
}
