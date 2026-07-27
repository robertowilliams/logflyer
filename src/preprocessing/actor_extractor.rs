//! Stage 12 — promote agents, skills and resources to graph nodes.
//!
//! Before this stage the graph contained only *events*: `PromptEvent`,
//! `ToolCallEvent`, `AgentStep` and so on. The participants were buried as
//! attributes on those events — `model_id`, `tool_name`, `mcp_server_id` — which
//! meant "which agents used this skill" was not a traversal, it was a scan.
//!
//! This stage emits an [`ActorRecord`] per distinct participant and an edge from
//! each event to the actors it involved, so the interaction graph an audit wants
//! actually has the participants in it.
//!
//! # Actors are cross-sample; events are not
//!
//! [`ids::derive_actor_id`] keys on `(kind, name)` rather than `sample_hash`, so
//! the model `claude-opus-4` is one node across every sample it appears in. That
//! is the whole point — a per-sample actor id would give one disconnected node per
//! occurrence and answer nothing.
//!
//! # Why these are not `EntityType` variants
//!
//! See [`ActorRecord`]. In short: `otel_builder` emits one span per entity, so
//! actors-as-entities would fabricate a span for every agent and tool; and an
//! `EntityRecord` is a parsed log line, which an actor is not.

use std::collections::{HashMap, HashSet};

use mongodb::bson::DateTime;

use super::{ids, task_correlator};
use crate::models::{
    ActorKind, ActorRecord, EntityRecord, RelationEdge, RelationSource, RelationType,
};

/// Everything Stage 12 produces for one sample.
#[derive(Debug, Default)]
pub struct ActorExtraction {
    /// Distinct actors seen in this sample.
    pub actors: Vec<ActorRecord>,
    /// Event → actor edges, ready to go into `entity_edges` alongside the rest.
    pub edges: Vec<RelationEdge>,
}

/// Build actors and their edges from a sample's entities.
///
/// Actors are deduplicated within the sample: three tool calls to `web_search`
/// produce one `Skill` node and three `USED_SKILL` edges, not three nodes.
///
/// `task_id` is recorded on each actor so the `actors` collection can answer
/// "which agents worked on this task" directly, without joining through samples.
pub fn extract(entities: &[EntityRecord], sample_hash: &str, task_id: &str) -> ActorExtraction {
    // actor_id → record, so repeated references merge into one node.
    let mut actors: HashMap<String, ActorRecord> = HashMap::new();
    let mut edges: Vec<RelationEdge> = Vec::new();
    let now = DateTime::now();

    for entity in entities {
        // Actor ids already seen on *this* event.
        //
        // `candidates` emits an Agent for `model_id` and another for
        // `agent_id`, which is right when they name different things but wrong
        // when they carry the same string — `agent=claude-3-opus
        // model=claude-3-opus` is one participant described twice, not two. Left
        // undeduplicated it counted the event twice and emitted two edges with
        // an identical `relation_id`, which then inflated the task's
        // `relation_count`.
        let mut seen_on_event: HashSet<String> = HashSet::new();

        // An event can involve all three kinds at once — an MCP tool call made by
        // a named model touches an agent, a skill and a resource.
        for (kind, name, field, relation) in candidates(entity) {
            let Some(name) = normalise(name) else { continue };
            let actor_id = ids::derive_actor_id(kind.as_str(), &name);
            if !seen_on_event.insert(actor_id.clone()) {
                continue;
            }

            let record = actors.entry(actor_id.clone()).or_insert_with(|| ActorRecord {
                actor_id: actor_id.clone(),
                kind,
                name: name.clone(),
                source_field: field.to_string(),
                sample_hashes: vec![sample_hash.to_string()],
                task_ids: if task_id.is_empty() {
                    Vec::new()
                } else {
                    vec![task_id.to_string()]
                },
                event_count: 0,
                first_seen: now,
                last_seen: now,
            });
            record.event_count += 1;

            edges.push(make_edge(relation, &entity.entity_id, &actor_id, sample_hash));
        }
    }

    ActorExtraction {
        actors: actors.into_values().collect(),
        edges,
    }
}

/// The actor references carried by one event, in a fixed order.
///
/// `model_id` is preferred over `agent_id` for the Agent node because a model is
/// the more specific identity — `claude-opus-4` says more than `researcher`. Both
/// are emitted when both are present, since they are genuinely different
/// participants: the named agent role and the model backing it.
fn candidates(
    entity: &EntityRecord,
) -> Vec<(ActorKind, Option<&str>, &'static str, RelationType)> {
    let agent_field = entity
        .extracted_fields
        .get("agent_id")
        .or_else(|| entity.extracted_fields.get("agent"))
        .and_then(|v| v.as_str());

    vec![
        (
            ActorKind::Agent,
            entity.model_id.as_deref(),
            "model_id",
            RelationType::PerformedBy,
        ),
        (
            ActorKind::Agent,
            agent_field,
            "agent_id",
            RelationType::PerformedBy,
        ),
        (
            ActorKind::Skill,
            entity.tool_name.as_deref(),
            "tool_name",
            RelationType::UsedSkill,
        ),
        (
            ActorKind::Resource,
            entity.mcp_server_id.as_deref(),
            "mcp_server_id",
            RelationType::AccessedResource,
        ),
    ]
}

/// Trim and reject names that carry no identity.
///
/// A blank or placeholder name would collapse every event that shares the
/// emptiness into one meaningless hub node — the same failure the task correlator
/// guards against.
/// Uses the same placeholder list as the task correlator. The two were separate
/// and had drifted: this one was missing `<nil>`, so a Go log's
/// `tool_name=<nil>` became a Skill node literally named `<nil>`.
fn normalise(name: Option<&str>) -> Option<String> {
    let trimmed = name?.trim();
    if trimmed.is_empty() || task_correlator::is_placeholder(&trimmed.to_ascii_lowercase()) {
        return None;
    }
    Some(trimmed.to_string())
}

/// Build an event → actor edge.
///
/// Confidence is `1.0` / [`RelationSource::Explicit`]: the actor's name was read
/// from a field on the event, not inferred from position.
fn make_edge(
    relation_type: RelationType,
    source_entity_id: &str,
    actor_id: &str,
    sample_hash: &str,
) -> RelationEdge {
    let relation_id = ids::derive_relation_id(
        sample_hash,
        &format!("{relation_type:?}"),
        source_entity_id,
        actor_id,
    );
    RelationEdge {
        relation_id,
        relation_type,
        source_entity_id: source_entity_id.to_string(),
        target_entity_id: actor_id.to_string(),
        sample_hash: sample_hash.to_string(),
        confidence: 1.0,
        source: RelationSource::Explicit,
        created_at: DateTime::now(),
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{EntityType, SemanticRole};

    fn entity(id: &str) -> EntityRecord {
        EntityRecord {
            entity_id: id.to_string(),
            entity_type: EntityType::ToolCallEvent,
            semantic_role: SemanticRole::Unknown,
            sample_hash: "h".to_string(),
            target_id: "t".to_string(),
            trace_id: "trace".to_string(),
            span_id: "span".to_string(),
            parent_span_id: None,
            prov_entity_id: format!("ug:entity:{id}"),
            prov_activity_id: "ug:activity:h:0".to_string(),
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

    fn with_tool(id: &str, tool: &str) -> EntityRecord {
        let mut e = entity(id);
        e.tool_name = Some(tool.to_string());
        e
    }

    fn kinds(x: &ActorExtraction) -> Vec<(ActorKind, &str)> {
        let mut v: Vec<(ActorKind, &str)> =
            x.actors.iter().map(|a| (a.kind, a.name.as_str())).collect();
        v.sort_by(|a, b| a.1.cmp(b.1));
        v
    }

    // ── One participant described twice is still one participant ─────────────

    #[test]
    fn a_model_and_agent_with_the_same_name_are_one_actor() {
        // `agent=claude-3-opus model=claude-3-opus` is common — an agent named
        // after its model. `candidates` emits an Agent for each field, and
        // undeduplicated they both resolved to the same actor_id.
        let mut e = entity("e1");
        e.model_id = Some("claude-3-opus".to_string());
        e.extracted_fields
            .insert("agent".to_string(), serde_json::json!("claude-3-opus"));

        let x = extract(&[e], "h", "task");

        assert_eq!(x.actors.len(), 1, "one name, one node");
        assert_eq!(
            x.actors[0].event_count, 1,
            "one event must count once, not once per field naming the same actor",
        );
        assert_eq!(x.edges.len(), 1, "and must not emit two edges");
    }

    #[test]
    fn duplicate_edges_never_share_a_relation_id() {
        // The concrete corruption: `derive_relation_id` keys on
        // (sample_hash, type, source, target), so two edges for one
        // (event, actor, PerformedBy) pair are byte-identical rows that both
        // ride in `metadata.relations` and inflate the task's relation_count.
        let mut e = entity("e1");
        e.model_id = Some("dup".to_string());
        e.extracted_fields
            .insert("agent_id".to_string(), serde_json::json!("dup"));

        let x = extract(&[e], "h", "task");
        let mut ids: Vec<&str> = x.edges.iter().map(|r| r.relation_id.as_str()).collect();
        let before = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), before, "relation ids must be unique");
    }

    #[test]
    fn a_model_and_a_differently_named_agent_stay_two_actors() {
        // The dedup must not collapse the case it was never about: a named role
        // and the model backing it are genuinely two participants.
        let mut e = entity("e1");
        e.model_id = Some("claude-3-opus".to_string());
        e.extracted_fields
            .insert("agent".to_string(), serde_json::json!("researcher"));

        let x = extract(&[e], "h", "task");
        assert_eq!(x.actors.len(), 2);
        assert_eq!(x.edges.len(), 2);
    }

    #[test]
    fn the_same_actor_on_two_events_counts_twice() {
        // Dedup is per event, not per sample — two tool calls to one skill are
        // one node with two edges and an event_count of 2.
        let x = extract(&[with_tool("e1", "search"), with_tool("e2", "search")], "h", "task");
        assert_eq!(x.actors.len(), 1);
        assert_eq!(x.actors[0].event_count, 2);
        assert_eq!(x.edges.len(), 2);
    }

    #[test]
    fn go_style_nil_is_not_a_skill() {
        // The actor placeholder list was missing `<nil>` while the correlator's
        // had it, so a Go log produced a Skill node literally named `<nil>`.
        let x = extract(&[with_tool("e1", "<nil>")], "h", "task");
        assert!(x.actors.is_empty(), "got {:?}", kinds(&x));
        assert!(x.edges.is_empty());
    }

    // ── Derivation ───────────────────────────────────────────────────────────

    #[test]
    fn a_tool_name_becomes_a_skill_node() {
        let x = extract(&[with_tool("e1", "web_search")], "h", "task-1");
        assert_eq!(kinds(&x), vec![(ActorKind::Skill, "web_search")]);
        assert_eq!(x.edges.len(), 1);
        assert_eq!(x.edges[0].relation_type, RelationType::UsedSkill);
        assert_eq!(x.edges[0].source_entity_id, "e1");
        assert_eq!(x.edges[0].target_entity_id, x.actors[0].actor_id);
    }

    #[test]
    fn a_model_id_becomes_an_agent_node() {
        let mut e = entity("e1");
        e.model_id = Some("claude-opus-4".to_string());
        let x = extract(&[e], "h", "task-1");
        assert_eq!(kinds(&x), vec![(ActorKind::Agent, "claude-opus-4")]);
        assert_eq!(x.edges[0].relation_type, RelationType::PerformedBy);
    }

    #[test]
    fn an_mcp_server_becomes_a_resource_node() {
        let mut e = entity("e1");
        e.mcp_server_id = Some("acme-mcp-server".to_string());
        let x = extract(&[e], "h", "task-1");
        assert_eq!(kinds(&x), vec![(ActorKind::Resource, "acme-mcp-server")]);
        assert_eq!(x.edges[0].relation_type, RelationType::AccessedResource);
    }

    #[test]
    fn one_event_can_involve_all_three_kinds() {
        // An MCP tool call made by a named model touches an agent, a skill and a
        // resource simultaneously.
        let mut e = entity("e1");
        e.model_id = Some("claude-opus-4".to_string());
        e.tool_name = Some("web_search".to_string());
        e.mcp_server_id = Some("acme".to_string());
        let x = extract(&[e], "h", "task-1");
        assert_eq!(x.actors.len(), 3);
        assert_eq!(x.edges.len(), 3);
    }

    #[test]
    fn an_agent_field_is_recognised_alongside_model_id() {
        // crewai names agents in `agent=researcher` with no model on the line.
        let mut e = entity("e1");
        e.extracted_fields
            .insert("agent".to_string(), serde_json::json!("researcher"));
        let x = extract(&[e], "h", "task-1");
        assert_eq!(kinds(&x), vec![(ActorKind::Agent, "researcher")]);
    }

    // ── Deduplication ────────────────────────────────────────────────────────

    #[test]
    fn repeated_references_make_one_node_and_many_edges() {
        // Three calls to the same tool: one Skill node, three edges.
        let x = extract(
            &[
                with_tool("e1", "web_search"),
                with_tool("e2", "web_search"),
                with_tool("e3", "web_search"),
            ],
            "h",
            "task-1",
        );
        assert_eq!(x.actors.len(), 1, "one node per distinct skill");
        assert_eq!(x.actors[0].event_count, 3, "but the references are counted");
        assert_eq!(x.edges.len(), 3);
    }

    #[test]
    fn distinct_names_make_distinct_nodes() {
        let x = extract(
            &[with_tool("e1", "web_search"), with_tool("e2", "file_writer")],
            "h",
            "task-1",
        );
        assert_eq!(x.actors.len(), 2);
    }

    #[test]
    fn the_same_actor_gets_the_same_id_in_a_different_sample() {
        // The cross-sample property that makes the whole stage worthwhile.
        let a = extract(&[with_tool("e1", "web_search")], "hash-a", "task-1");
        let b = extract(&[with_tool("e9", "web_search")], "hash-b", "task-2");
        assert_eq!(a.actors[0].actor_id, b.actors[0].actor_id);
    }

    #[test]
    fn an_agent_and_a_skill_sharing_a_name_stay_separate() {
        let mut e = entity("e1");
        e.model_id = Some("search".to_string());
        e.tool_name = Some("search".to_string());
        let x = extract(&[e], "h", "task-1");
        assert_eq!(x.actors.len(), 2, "same name, different kinds, two nodes");
    }

    // ── Edge properties ──────────────────────────────────────────────────────

    #[test]
    fn edges_are_explicit_not_inferred() {
        // The actor name was read from a field, not guessed from position.
        let x = extract(&[with_tool("e1", "web_search")], "h", "task-1");
        assert_eq!(x.edges[0].confidence, 1.0);
        assert_eq!(x.edges[0].source, RelationSource::Explicit);
    }

    #[test]
    fn edge_ids_are_deterministic_across_runs() {
        let a = extract(&[with_tool("e1", "web_search")], "h", "task-1");
        let b = extract(&[with_tool("e1", "web_search")], "h", "task-1");
        assert_eq!(a.edges[0].relation_id, b.edges[0].relation_id);
    }

    #[test]
    fn edges_of_different_kinds_do_not_share_an_id() {
        let mut e = entity("e1");
        e.model_id = Some("x".to_string());
        e.tool_name = Some("x".to_string());
        let x = extract(&[e], "h", "task-1");
        let ids: std::collections::HashSet<&str> =
            x.edges.iter().map(|e| e.relation_id.as_str()).collect();
        assert_eq!(ids.len(), 2, "relation ids must be distinct");
    }

    // ── Rejection ────────────────────────────────────────────────────────────

    #[test]
    fn blank_and_placeholder_names_are_rejected() {
        // A blank name would collapse every event sharing it into one hub node.
        for bad in ["", "   ", "null", "NONE", "unknown", "-", "n/a", "undefined"] {
            let x = extract(&[with_tool("e1", bad)], "h", "task-1");
            assert!(x.actors.is_empty(), "tool_name={bad:?} must not become a node");
            assert!(x.edges.is_empty());
        }
    }

    #[test]
    fn names_are_trimmed() {
        let a = extract(&[with_tool("e1", "  web_search  ")], "h", "t");
        let b = extract(&[with_tool("e2", "web_search")], "h", "t");
        assert_eq!(a.actors[0].actor_id, b.actors[0].actor_id);
    }

    #[test]
    fn entities_with_no_actor_fields_produce_nothing() {
        let x = extract(&[entity("e1"), entity("e2")], "h", "task-1");
        assert!(x.actors.is_empty());
        assert!(x.edges.is_empty());
    }

    #[test]
    fn no_entities_at_all_is_not_an_error() {
        let x = extract(&[], "h", "task-1");
        assert!(x.actors.is_empty());
        assert!(x.edges.is_empty());
    }

    // ── Task linkage ─────────────────────────────────────────────────────────

    #[test]
    fn actors_record_the_task_they_participated_in() {
        let x = extract(&[with_tool("e1", "web_search")], "h", "task-42");
        assert_eq!(x.actors[0].task_ids, vec!["task-42".to_string()]);
        assert_eq!(x.actors[0].sample_hashes, vec!["h".to_string()]);
    }

    #[test]
    fn an_empty_task_id_is_not_recorded() {
        // Task correlation can be switched off independently.
        let x = extract(&[with_tool("e1", "web_search")], "h", "");
        assert!(x.actors[0].task_ids.is_empty());
    }
}
