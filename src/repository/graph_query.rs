//! Graph traversal primitives over the `entity_edges` collection.
//!
//! The `entity_edges` collection is an adjacency list: each document is one
//! [`RelationEdge`] with a `source_entity_id` and a `target_entity_id`, both
//! indexed (`ee_source_entity_id` / `ee_target_entity_id`).  That makes
//! level-at-a-time breadth-first search cheap — one indexed `$in` query per
//! depth level rather than one per node.
//!
//! This module deliberately splits the *algorithm* from the *storage*:
//!
//! * [`Traversal`] holds the BFS bookkeeping (visited set, frontier, collected
//!   edges, budget accounting) and knows nothing about MongoDB.
//! * [`MongoRepository`] drives it, supplying each level's edges from the
//!   database.
//!
//! The split exists so the traversal logic is unit-testable without a live
//! Mongo instance — see the tests at the bottom of this file, which feed
//! [`Traversal`] from an in-memory edge list.
//!
//! [`RelationEdge`]: crate::models::RelationEdge
//! [`MongoRepository`]: super::MongoRepository

use std::collections::{HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};

// ─── Direction ────────────────────────────────────────────────────────────────

/// Which way to walk the directed edge set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Follow `source_entity_id → target_entity_id`: "what did this cause?"
    Downstream,
    /// Follow `target_entity_id → source_entity_id`: "what caused this?"
    Upstream,
}

impl Direction {
    /// The BSON field to match the current frontier against.
    pub fn match_field(self) -> &'static str {
        match self {
            Direction::Downstream => "source_entity_id",
            Direction::Upstream => "target_entity_id",
        }
    }

    /// The BSON field holding the entity we advance to.
    pub fn advance_field(self) -> &'static str {
        match self {
            Direction::Downstream => "target_entity_id",
            Direction::Upstream => "source_entity_id",
        }
    }
}

// ─── Limits ───────────────────────────────────────────────────────────────────

/// Hard ceiling on `?depth=`, regardless of what the caller asks for.
///
/// Relation graphs for a single sample are shallow (a prompt → completion →
/// tool-call chain is rarely more than a handful of hops), so a request for
/// depth 50 is a mistake or an attack rather than a real query.
pub const MAX_DEPTH: u32 = 10;

/// Hard ceiling on the number of distinct entities a single traversal may
/// visit.  Reaching it sets `truncated` on the response rather than erroring —
/// a partial graph is more useful to the UI than a failure.
pub const MAX_NODES: usize = 5_000;

/// Hard ceiling on the number of edges a single traversal may collect.
///
/// A node budget alone does not bound the response: one hub entity with a huge
/// fan-out can produce far more edges than nodes, and the graph is only
/// readable at a fraction of this size anyway.  Both budgets are needed.
pub const MAX_EDGES: usize = 20_000;

/// Clamp a caller-supplied depth into `1..=MAX_DEPTH`.
pub fn clamp_depth(requested: u32) -> u32 {
    requested.clamp(1, MAX_DEPTH)
}

// ─── Minimal edge view ────────────────────────────────────────────────────────

/// The two endpoints of an edge, plus its identity.
///
/// Traversal only needs the endpoints; the full edge document is carried
/// separately so the API can return it verbatim without this module having to
/// know the shape of a [`RelationEdge`].
///
/// [`RelationEdge`]: crate::models::RelationEdge
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdgeEndpoints {
    pub relation_id: String,
    pub source_entity_id: String,
    pub target_entity_id: String,
}

impl EdgeEndpoints {
    /// The endpoint we arrive at when traversing this edge in `dir`.
    pub fn advance_to(&self, dir: Direction) -> &str {
        match dir {
            Direction::Downstream => &self.target_entity_id,
            Direction::Upstream => &self.source_entity_id,
        }
    }

    /// The endpoint that must be in the frontier for this edge to apply.
    pub fn matched_on(&self, dir: Direction) -> &str {
        match dir {
            Direction::Downstream => &self.source_entity_id,
            Direction::Upstream => &self.target_entity_id,
        }
    }
}

// ─── Traversal state machine ──────────────────────────────────────────────────

/// Breadth-first traversal bookkeeping, driven one level at a time.
///
/// Usage is a loop: [`frontier`](Self::frontier) yields the ids to query,
/// [`absorb_level`](Self::absorb_level) takes the edges found for those ids and
/// computes the next frontier, and [`is_done`](Self::is_done) says when to
/// stop.
///
/// # Example
/// ```rust,ignore
/// let mut t = Traversal::new("abc123", Direction::Downstream, 3);
/// while !t.is_done() {
///     let edges = fetch_edges_for(t.frontier(), t.direction()).await?;
///     t.absorb_level(&edges);
/// }
/// let visited = t.into_visited();
/// ```
#[derive(Debug)]
pub struct Traversal {
    direction: Direction,
    max_depth: u32,
    /// Depth levels already absorbed.
    depth_done: u32,
    /// Ids to query on the next level.
    frontier: Vec<String>,
    /// Every id seen, including the root.
    visited: HashSet<String>,
    /// Insertion-ordered visited ids, so output is deterministic.
    visited_order: Vec<String>,
    /// `relation_id`s already collected, to avoid returning an edge twice when
    /// two frontier nodes share a neighbour.
    seen_edges: HashSet<String>,
    /// Collected `relation_id`s in discovery order.
    edge_ids: Vec<String>,
    /// Set when the node or edge budget stopped the walk early.
    truncated: bool,
    /// Depth at which the last *new* node was discovered.  Distinct from
    /// `depth_done`: a final level that finds nothing must not inflate it.
    deepest: u32,
}

impl Traversal {
    /// Start a traversal rooted at `root`.
    ///
    /// `max_depth` is clamped to [`MAX_DEPTH`]; the root counts as depth 0 and
    /// is always present in the visited set even when it has no edges.
    pub fn new(root: &str, direction: Direction, max_depth: u32) -> Self {
        let root = root.to_string();
        let mut visited = HashSet::new();
        visited.insert(root.clone());
        Self {
            direction,
            max_depth: clamp_depth(max_depth),
            depth_done: 0,
            frontier: vec![root.clone()],
            visited,
            visited_order: vec![root],
            seen_edges: HashSet::new(),
            edge_ids: Vec::new(),
            truncated: false,
            deepest: 0,
        }
    }

    pub fn direction(&self) -> Direction {
        self.direction
    }

    /// Ids to match against [`Direction::match_field`] on the next query.
    pub fn frontier(&self) -> &[String] {
        &self.frontier
    }

    /// True when the depth limit is reached, the frontier is empty, or the node
    /// budget was exhausted.
    pub fn is_done(&self) -> bool {
        self.depth_done >= self.max_depth || self.frontier.is_empty() || self.truncated
    }

    /// Whether the walk stopped early because of [`MAX_NODES`] or
    /// [`MAX_EDGES`].
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// Mark the walk as truncated by an externally-enforced budget.
    ///
    /// Used by the storage layer when a per-level edge fetch hits
    /// [`MAX_EDGES`], since that ceiling is applied while draining the cursor —
    /// before the edges ever reach [`absorb_level`](Self::absorb_level).
    pub fn mark_truncated(&mut self) {
        self.truncated = true;
    }

    /// Hop distance of the furthest node discovered.
    ///
    /// This is the depth of the deepest *node*, not the number of query rounds:
    /// a final level that discovers nothing does not increase it, so a 3-hop
    /// chain walked with `depth=9` reports 3.
    pub fn depth_reached(&self) -> u32 {
        self.deepest
    }

    /// Collected `relation_id`s, in discovery order.
    pub fn edge_ids(&self) -> &[String] {
        &self.edge_ids
    }

    /// Every visited entity id, root first, then in discovery order.
    pub fn visited(&self) -> &[String] {
        &self.visited_order
    }

    /// Consume the traversal for its visited ids.
    pub fn into_visited(self) -> Vec<String> {
        self.visited_order
    }

    /// Take one level's worth of edges and compute the next frontier.
    ///
    /// Edges whose matched endpoint is not in the current frontier are ignored,
    /// so a caller that over-fetches cannot corrupt the walk.
    ///
    /// When a budget is exhausted the walk stops admitting *new nodes* but keeps
    /// collecting edges between nodes already visited.  Those edges cost nothing
    /// against the node budget, and dropping them would render visited nodes as
    /// spuriously disconnected.
    pub fn absorb_level(&mut self, edges: &[EdgeEndpoints]) {
        let current: HashSet<&str> = self.frontier.iter().map(String::as_str).collect();
        let mut next: Vec<String> = Vec::new();

        for edge in edges {
            if !current.contains(edge.matched_on(self.direction)) {
                continue;
            }
            if self.edge_ids.len() >= MAX_EDGES {
                self.truncated = true;
                break;
            }
            if self.seen_edges.insert(edge.relation_id.clone()) {
                self.edge_ids.push(edge.relation_id.clone());
            }

            let neighbour = edge.advance_to(self.direction);
            if self.visited.contains(neighbour) {
                continue;
            }
            if self.visited.len() >= MAX_NODES {
                // Out of node budget: keep scanning so edges among already
                // visited nodes are still collected, but admit nothing new.
                self.truncated = true;
                continue;
            }
            self.visited.insert(neighbour.to_string());
            self.visited_order.push(neighbour.to_string());
            next.push(neighbour.to_string());
        }

        self.depth_done += 1;
        // Only a level that actually discovered nodes extends the graph's depth.
        if !next.is_empty() {
            self.deepest = self.depth_done;
        }
        self.frontier = next;
    }
}

// ─── Shortest path ────────────────────────────────────────────────────────────

/// One hop in a resolved path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PathHop {
    pub relation_id: String,
    pub from: String,
    pub to: String,
}

/// Breadth-first shortest path over an in-memory edge set.
///
/// BFS (not Dijkstra) because every edge costs the same one hop — `confidence`
/// describes how much we trust the edge, not how far it goes, so weighting by
/// it would conflate two different things.
///
/// Returns `None` when `to` is unreachable from `from` within `max_depth` hops.
/// A zero-length path (`from == to`) yields `Some(vec![])`.
pub fn shortest_path(
    edges: &[EdgeEndpoints],
    from: &str,
    to: &str,
    max_depth: u32,
) -> Option<Vec<PathHop>> {
    if from == to {
        return Some(Vec::new());
    }
    let max_depth = clamp_depth(max_depth);

    // Adjacency: source → [(target, relation_id)]
    let mut adjacency: HashMap<&str, Vec<(&str, &str)>> = HashMap::new();
    for e in edges {
        adjacency
            .entry(e.source_entity_id.as_str())
            .or_default()
            .push((e.target_entity_id.as_str(), e.relation_id.as_str()));
    }

    // node → (predecessor, relation_id used to reach it)
    let mut came_from: HashMap<&str, (&str, &str)> = HashMap::new();
    let mut visited: HashSet<&str> = HashSet::from([from]);
    let mut queue: VecDeque<(&str, u32)> = VecDeque::from([(from, 0u32)]);

    while let Some((node, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        for &(neighbour, relation_id) in adjacency.get(node).into_iter().flatten() {
            if !visited.insert(neighbour) {
                continue;
            }
            came_from.insert(neighbour, (node, relation_id));
            if neighbour == to {
                return Some(reconstruct(&came_from, from, to));
            }
            queue.push_back((neighbour, depth + 1));
        }
    }
    None
}

/// Walk `came_from` backwards from `to` to `from`, then reverse.
fn reconstruct(came_from: &HashMap<&str, (&str, &str)>, from: &str, to: &str) -> Vec<PathHop> {
    let mut hops = Vec::new();
    let mut cursor = to;
    while cursor != from {
        let Some(&(prev, relation_id)) = came_from.get(cursor) else {
            break;
        };
        hops.push(PathHop {
            relation_id: relation_id.to_string(),
            from: prev.to_string(),
            to: cursor.to_string(),
        });
        cursor = prev;
    }
    hops.reverse();
    hops
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(id: &str, src: &str, tgt: &str) -> EdgeEndpoints {
        EdgeEndpoints {
            relation_id: id.to_string(),
            source_entity_id: src.to_string(),
            target_entity_id: tgt.to_string(),
        }
    }

    /// a → b → c → d, plus a side branch b → e.
    fn chain() -> Vec<EdgeEndpoints> {
        vec![
            edge("r1", "a", "b"),
            edge("r2", "b", "c"),
            edge("r3", "c", "d"),
            edge("r4", "b", "e"),
        ]
    }

    /// Drive a traversal to completion against an in-memory edge list.
    fn walk(edges: &[EdgeEndpoints], root: &str, dir: Direction, depth: u32) -> Traversal {
        let mut t = Traversal::new(root, dir, depth);
        while !t.is_done() {
            let frontier: HashSet<&str> = t.frontier().iter().map(String::as_str).collect();
            let level: Vec<EdgeEndpoints> = edges
                .iter()
                .filter(|e| frontier.contains(e.matched_on(dir)))
                .cloned()
                .collect();
            t.absorb_level(&level);
        }
        t
    }

    // ── Direction field mapping ──────────────────────────────────────────────

    #[test]
    fn direction_fields_are_mirrored() {
        assert_eq!(Direction::Downstream.match_field(), "source_entity_id");
        assert_eq!(Direction::Downstream.advance_field(), "target_entity_id");
        assert_eq!(Direction::Upstream.match_field(), "target_entity_id");
        assert_eq!(Direction::Upstream.advance_field(), "source_entity_id");
    }

    #[test]
    fn direction_serializes_snake_case() {
        let json = serde_json::to_string(&Direction::Downstream).unwrap();
        assert_eq!(json, "\"downstream\"");
    }

    // ── Depth clamping ───────────────────────────────────────────────────────

    #[test]
    fn depth_zero_clamps_to_one() {
        assert_eq!(clamp_depth(0), 1);
    }

    #[test]
    fn depth_above_ceiling_clamps_down() {
        assert_eq!(clamp_depth(999), MAX_DEPTH);
    }

    // ── Downstream traversal ─────────────────────────────────────────────────

    #[test]
    fn depth_one_reaches_direct_neighbours_only() {
        let t = walk(&chain(), "a", Direction::Downstream, 1);
        assert_eq!(t.visited(), &["a", "b"]);
        assert_eq!(t.edge_ids(), &["r1"]);
    }

    #[test]
    fn depth_two_reaches_the_branch_and_the_chain() {
        let t = walk(&chain(), "a", Direction::Downstream, 2);
        assert_eq!(t.visited(), &["a", "b", "c", "e"]);
        assert_eq!(t.edge_ids(), &["r1", "r2", "r4"]);
    }

    #[test]
    fn traversal_stops_when_frontier_empties_before_depth_limit() {
        // Depth 9 requested, but the graph is only 3 hops deep.
        let t = walk(&chain(), "a", Direction::Downstream, 9);
        assert_eq!(t.visited().len(), 5); // a, b, c, e, d
        assert_eq!(t.edge_ids().len(), 4);
        assert!(!t.truncated());
    }

    // ── depth_reached reports node distance, not query rounds ─────────────────

    #[test]
    fn depth_reached_is_the_distance_to_the_furthest_node() {
        // d is exactly 3 hops from a. The walk needs a 4th round to discover the
        // frontier is empty, but that round must not inflate the reported depth.
        let t = walk(&chain(), "a", Direction::Downstream, 9);
        assert_eq!(t.depth_reached(), 3);
    }

    #[test]
    fn depth_reached_is_zero_for_an_isolated_root() {
        let t = walk(&chain(), "orphan", Direction::Downstream, 5);
        assert_eq!(t.depth_reached(), 0);
    }

    #[test]
    fn depth_reached_does_not_count_a_revisit_only_level() {
        // a → b → a: the second level re-finds `a`, discovering no new node.
        let edges = vec![edge("r1", "a", "b"), edge("r2", "b", "a")];
        let t = walk(&edges, "a", Direction::Downstream, 5);
        assert_eq!(t.depth_reached(), 1, "b is the only node past the root");
    }

    #[test]
    fn depth_reached_is_capped_by_the_requested_depth() {
        let t = walk(&chain(), "a", Direction::Downstream, 2);
        assert_eq!(t.depth_reached(), 2);
    }

    // ── Budgets ──────────────────────────────────────────────────────────────

    #[test]
    fn node_budget_still_collects_edges_between_visited_nodes() {
        // Two frontier nodes both pointing at an already-visited node: even if
        // the node budget were spent, neither edge costs a new node, so both
        // must survive.
        let edges = vec![
            edge("r1", "a", "b"),
            edge("r2", "a", "c"),
            edge("r3", "b", "c"), // c already visited via r2
        ];
        let t = walk(&edges, "a", Direction::Downstream, 3);
        assert_eq!(t.edge_ids().len(), 3, "r3 connects two visited nodes");
        assert!(!t.truncated());
    }

    #[test]
    fn mark_truncated_ends_the_walk() {
        let mut t = Traversal::new("a", Direction::Downstream, 5);
        assert!(!t.is_done());
        t.mark_truncated();
        assert!(t.is_done());
        assert!(t.truncated());
    }

    #[test]
    fn root_with_no_edges_yields_only_itself() {
        let t = walk(&chain(), "orphan", Direction::Downstream, 3);
        assert_eq!(t.visited(), &["orphan"]);
        assert!(t.edge_ids().is_empty());
    }

    // ── Upstream traversal ───────────────────────────────────────────────────

    #[test]
    fn upstream_walks_edges_backwards() {
        let t = walk(&chain(), "d", Direction::Upstream, 2);
        assert_eq!(t.visited(), &["d", "c", "b"]);
        assert_eq!(t.edge_ids(), &["r3", "r2"]);
    }

    // ── Cycles and diamonds ──────────────────────────────────────────────────

    #[test]
    fn cycle_does_not_loop_forever() {
        let edges = vec![edge("r1", "a", "b"), edge("r2", "b", "a")];
        let t = walk(&edges, "a", Direction::Downstream, MAX_DEPTH);
        assert_eq!(t.visited(), &["a", "b"]);
        // Both edges are reported — the cycle is real data, worth showing.
        assert_eq!(t.edge_ids().len(), 2);
    }

    #[test]
    fn diamond_reports_the_join_node_once() {
        // a → b, a → c, b → d, c → d
        let edges = vec![
            edge("r1", "a", "b"),
            edge("r2", "a", "c"),
            edge("r3", "b", "d"),
            edge("r4", "c", "d"),
        ];
        let t = walk(&edges, "a", Direction::Downstream, 3);
        assert_eq!(t.visited(), &["a", "b", "c", "d"]);
        assert_eq!(t.edge_ids().len(), 4, "both paths into d are kept");
    }

    #[test]
    fn self_loop_is_collected_without_revisiting() {
        let edges = vec![edge("r1", "a", "a")];
        let t = walk(&edges, "a", Direction::Downstream, 3);
        assert_eq!(t.visited(), &["a"]);
        assert_eq!(t.edge_ids(), &["r1"]);
    }

    // ── Over-fetch protection ────────────────────────────────────────────────

    #[test]
    fn edges_outside_the_frontier_are_ignored() {
        let mut t = Traversal::new("a", Direction::Downstream, 3);
        // "c → d" does not touch the frontier ["a"] and must not be absorbed.
        t.absorb_level(&[edge("r1", "a", "b"), edge("r3", "c", "d")]);
        assert_eq!(t.visited(), &["a", "b"]);
        assert_eq!(t.edge_ids(), &["r1"]);
    }

    // ── Shortest path ────────────────────────────────────────────────────────

    #[test]
    fn path_to_self_is_empty() {
        assert_eq!(shortest_path(&chain(), "a", "a", 5), Some(Vec::new()));
    }

    #[test]
    fn path_follows_the_chain() {
        let path = shortest_path(&chain(), "a", "d", 5).expect("d is reachable from a");
        assert_eq!(
            path.iter().map(|h| h.relation_id.as_str()).collect::<Vec<_>>(),
            vec!["r1", "r2", "r3"],
        );
        assert_eq!(path.first().unwrap().from, "a");
        assert_eq!(path.last().unwrap().to, "d");
    }

    #[test]
    fn path_prefers_the_shorter_of_two_routes() {
        // Long way: a → x → y → d.  Short way: a → d.
        let mut edges = vec![
            edge("r1", "a", "x"),
            edge("r2", "x", "y"),
            edge("r3", "y", "d"),
        ];
        edges.push(edge("r_direct", "a", "d"));
        let path = shortest_path(&edges, "a", "d", 5).unwrap();
        assert_eq!(path.len(), 1);
        assert_eq!(path[0].relation_id, "r_direct");
    }

    #[test]
    fn path_respects_the_depth_budget() {
        // d is 3 hops from a, so a 2-hop budget must not find it.
        assert!(shortest_path(&chain(), "a", "d", 2).is_none());
        assert!(shortest_path(&chain(), "a", "d", 3).is_some());
    }

    #[test]
    fn path_returns_none_for_unreachable_target() {
        assert!(shortest_path(&chain(), "d", "a", 5).is_none(), "edges are directed");
        assert!(shortest_path(&chain(), "a", "nonexistent", 5).is_none());
    }

    #[test]
    fn path_terminates_on_a_cycle() {
        let edges = vec![
            edge("r1", "a", "b"),
            edge("r2", "b", "c"),
            edge("r3", "c", "a"),
        ];
        assert!(shortest_path(&edges, "a", "unreachable", MAX_DEPTH).is_none());
        assert_eq!(shortest_path(&edges, "a", "c", MAX_DEPTH).unwrap().len(), 2);
    }
}
