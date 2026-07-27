//! Similarity search primitives over the embedding collections.
//!
//! Mirrors the split in [`super::graph_query`]: the maths lives here with no
//! MongoDB dependency, and [`MongoRepository`] drives it by supplying candidate
//! records. That keeps the scoring unit-testable without a database.
//!
//! # What a "hit" is
//!
//! Embeddings are keyed per **sample**, not per entity — `embedding_id` is
//! derived from `(sample_hash, kind, model)`, so a sample has at most one content
//! vector and one behavioral vector. Search therefore answers "which samples
//! resemble this one", which is the behavioural-clustering question the plan set
//! out to support ("find all agent runs with a similar tool-use pattern"), not
//! "which log lines resemble this line".
//!
//! # Why in-process scoring
//!
//! MongoDB Atlas offers `$vectorSearch`, but it requires a vector index that only
//! Atlas provides — a self-hosted standalone `mongod`, which is what the dev
//! stack and most self-hosted deployments run, cannot serve it. Scanning and
//! scoring in process works everywhere. At the scale these collections reach (one
//! record per sample) a scan is cheap; [`MAX_SCAN`] bounds it regardless.
//!
//! [`MongoRepository`]: super::MongoRepository

use serde::Serialize;

/// Hard ceiling on how many embedding records a single search will score.
///
/// Reaching it sets `truncated` on the response rather than erroring — the top
/// matches from a partial scan are still useful, and silently returning fewer
/// results than exist would be worse than saying so.
pub const MAX_SCAN: usize = 50_000;

/// Default number of hits to return.
pub const DEFAULT_LIMIT: usize = 10;

/// Hard ceiling on `limit`, so one request cannot ask for the whole collection.
pub const MAX_LIMIT: usize = 200;

/// Clamp a caller-supplied limit into `1..=MAX_LIMIT`.
pub fn clamp_limit(requested: usize) -> usize {
    requested.clamp(1, MAX_LIMIT)
}

// ─── Scoring ──────────────────────────────────────────────────────────────────

/// Cosine similarity of two equal-length vectors, in `-1.0..=1.0`.
///
/// Returns `None` when the vectors have different lengths or either has zero
/// magnitude — both are cases where "similarity" has no meaning, and returning
/// `0.0` would let a mismatched query masquerade as a legitimate poor match.
///
/// A length mismatch is the likely failure mode in practice: behavioral vectors
/// are 36-dimensional and content vectors 1536, so querying one collection with
/// the other's vector produces exactly this.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> Option<f32> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }

    // f64 accumulation: 1536 products of f32 lose noticeable precision, and the
    // ordering of near-identical neighbours depends on it.
    let mut dot = 0.0f64;
    let mut norm_a = 0.0f64;
    let mut norm_b = 0.0f64;

    for (&x, &y) in a.iter().zip(b.iter()) {
        let (x, y) = (x as f64, y as f64);
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }

    if norm_a == 0.0 || norm_b == 0.0 {
        return None;
    }

    let similarity = dot / (norm_a.sqrt() * norm_b.sqrt());
    // Floating-point error can push an identical pair a hair past 1.0, which
    // looks like a bug to anyone reading the score.
    Some(similarity.clamp(-1.0, 1.0) as f32)
}

// ─── Ranking ──────────────────────────────────────────────────────────────────

/// One scored match.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ScoredHit {
    pub sample_hash: String,
    /// Set only for [`crate::embedding::EmbeddingKind::Task`] hits, where it — not
    /// `sample_hash` — is the thing the caller asked about.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub embedding_id: String,
    /// Cosine similarity in `-1.0..=1.0`; `1.0` is identical.
    pub score: f32,
    pub model: String,
}

/// A candidate pulled from storage, before scoring.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub sample_hash: String,
    /// Present for task-intent embeddings only.
    pub task_id: Option<String>,
    pub embedding_id: String,
    pub model: String,
    pub vector: Vec<f32>,
}

/// Outcome of ranking a candidate set.
#[derive(Debug)]
pub struct Ranking {
    pub hits: Vec<ScoredHit>,
    /// Candidates that were scored (i.e. had a comparable vector).
    pub scored: usize,
    /// Candidates skipped because their vector could not be compared — almost
    /// always a dimensionality mismatch.
    pub skipped: usize,
}

/// Score every candidate against `query` and return the best `limit`.
///
/// Candidates whose vector cannot be compared are counted in `skipped` rather
/// than scored as `0.0`, so a caller can tell "nothing is similar" from "you
/// queried with the wrong dimensionality".
///
/// Ties break on `sample_hash` so the ordering is deterministic — without it,
/// two samples with identical vectors would swap places between calls and
/// paginating would be unstable.
pub fn rank(query: &[f32], candidates: &[Candidate], limit: usize) -> Ranking {
    let mut hits: Vec<ScoredHit> = Vec::new();
    let mut skipped = 0usize;

    for candidate in candidates {
        match cosine_similarity(query, &candidate.vector) {
            Some(score) => hits.push(ScoredHit {
                sample_hash: candidate.sample_hash.clone(),
                task_id: candidate.task_id.clone(),
                embedding_id: candidate.embedding_id.clone(),
                score,
                model: candidate.model.clone(),
            }),
            None => skipped += 1,
        }
    }

    let scored = hits.len();
    hits.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.sample_hash.cmp(&b.sample_hash))
    });
    hits.truncate(clamp_limit(limit));

    Ranking { hits, scored, skipped }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(hash: &str, vector: Vec<f32>) -> Candidate {
        Candidate {
            sample_hash: hash.to_string(),
            task_id: None,
            embedding_id: format!("eid-{hash}"),
            model: "behavioral-v1".to_string(),
            vector,
        }
    }

    // ── cosine_similarity ────────────────────────────────────────────────────

    #[test]
    fn identical_vectors_score_one() {
        let v = vec![1.0, 2.0, 3.0];
        let s = cosine_similarity(&v, &v).unwrap();
        assert!((s - 1.0).abs() < 1e-6, "expected ~1.0, got {s}");
    }

    #[test]
    fn identical_vectors_never_exceed_one() {
        // Floating-point error can push this past 1.0 without the clamp, which
        // reads as a bug in the score.
        let v: Vec<f32> = (0..1536).map(|i| (i as f32) * 0.017).collect();
        let s = cosine_similarity(&v, &v).unwrap();
        assert!(s <= 1.0, "score {s} exceeds 1.0");
    }

    #[test]
    fn orthogonal_vectors_score_zero() {
        let s = cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]).unwrap();
        assert!(s.abs() < 1e-6, "expected ~0.0, got {s}");
    }

    #[test]
    fn opposite_vectors_score_minus_one() {
        let s = cosine_similarity(&[1.0, 2.0], &[-1.0, -2.0]).unwrap();
        assert!((s + 1.0).abs() < 1e-6, "expected ~-1.0, got {s}");
    }

    #[test]
    fn magnitude_does_not_affect_the_score() {
        // Cosine is scale-invariant: a vector and its multiple are identical in
        // direction. This is why the vectors are not normalised before storage.
        let a = [1.0, 2.0, 3.0];
        let b = [10.0, 20.0, 30.0];
        let s = cosine_similarity(&a, &b).unwrap();
        assert!((s - 1.0).abs() < 1e-6, "expected ~1.0, got {s}");
    }

    #[test]
    fn length_mismatch_is_none_not_zero() {
        // The realistic failure: a 36-dim behavioral vector against a 1536-dim
        // content vector. Scoring it 0.0 would look like a legitimate poor match.
        assert_eq!(cosine_similarity(&[1.0; 36], &[1.0; 1536]), None);
    }

    #[test]
    fn zero_vector_is_none() {
        // A zero vector has no direction, so it has no cosine to anything.
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 1.0]), None);
        assert_eq!(cosine_similarity(&[1.0, 1.0], &[0.0, 0.0]), None);
    }

    #[test]
    fn empty_vectors_are_none() {
        assert_eq!(cosine_similarity(&[], &[]), None);
    }

    #[test]
    fn a_behavioral_sized_all_zero_vector_is_none() {
        // Real behavioral vectors can legitimately be all-zero for a sample with
        // no agentic content, so this case does occur.
        assert_eq!(cosine_similarity(&[0.0; 36], &[0.5; 36]), None);
    }

    // ── clamp_limit ──────────────────────────────────────────────────────────

    #[test]
    fn limit_zero_clamps_to_one() {
        assert_eq!(clamp_limit(0), 1);
    }

    #[test]
    fn limit_above_ceiling_clamps_down() {
        assert_eq!(clamp_limit(100_000), MAX_LIMIT);
    }

    // ── rank ─────────────────────────────────────────────────────────────────

    #[test]
    fn hits_come_back_best_first() {
        let query = [1.0, 0.0];
        let candidates = vec![
            candidate("orthogonal", vec![0.0, 1.0]),
            candidate("identical", vec![1.0, 0.0]),
            candidate("close", vec![0.9, 0.1]),
        ];
        let r = rank(&query, &candidates, 10);
        assert_eq!(
            r.hits.iter().map(|h| h.sample_hash.as_str()).collect::<Vec<_>>(),
            vec!["identical", "close", "orthogonal"],
        );
        assert_eq!(r.scored, 3);
        assert_eq!(r.skipped, 0);
    }

    #[test]
    fn limit_truncates_after_sorting() {
        // The best hit must survive truncation even when it is last in input.
        let candidates = vec![
            candidate("worst", vec![0.0, 1.0]),
            candidate("best", vec![1.0, 0.0]),
        ];
        let r = rank(&[1.0, 0.0], &candidates, 1);
        assert_eq!(r.hits.len(), 1);
        assert_eq!(r.hits[0].sample_hash, "best");
    }

    #[test]
    fn ties_break_deterministically_on_hash() {
        // Identical vectors must not swap order between calls, or pagination is
        // unstable.
        let candidates = vec![
            candidate("zzz", vec![1.0, 1.0]),
            candidate("aaa", vec![1.0, 1.0]),
            candidate("mmm", vec![1.0, 1.0]),
        ];
        let first = rank(&[1.0, 1.0], &candidates, 10);
        let second = rank(&[1.0, 1.0], &candidates, 10);
        assert_eq!(
            first.hits.iter().map(|h| h.sample_hash.as_str()).collect::<Vec<_>>(),
            vec!["aaa", "mmm", "zzz"],
        );
        assert_eq!(first.hits, second.hits, "ranking must be stable");
    }

    #[test]
    fn uncomparable_candidates_are_skipped_not_scored_zero() {
        let candidates = vec![
            candidate("good", vec![1.0, 0.0]),
            candidate("wrong_dims", vec![1.0, 0.0, 0.0]),
            candidate("all_zero", vec![0.0, 0.0]),
        ];
        let r = rank(&[1.0, 0.0], &candidates, 10);
        assert_eq!(r.scored, 1);
        assert_eq!(r.skipped, 2);
        assert_eq!(r.hits.len(), 1);
        assert_eq!(r.hits[0].sample_hash, "good");
    }

    #[test]
    fn every_candidate_skipped_yields_no_hits() {
        // Distinguishable from "nothing similar": scored == 0 with skipped > 0
        // means the query dimensionality was wrong.
        let candidates = vec![candidate("a", vec![1.0; 1536])];
        let r = rank(&[1.0; 36], &candidates, 10);
        assert!(r.hits.is_empty());
        assert_eq!(r.scored, 0);
        assert_eq!(r.skipped, 1);
    }

    #[test]
    fn empty_candidate_set_is_not_an_error() {
        let r = rank(&[1.0, 0.0], &[], 10);
        assert!(r.hits.is_empty());
        assert_eq!(r.scored, 0);
        assert_eq!(r.skipped, 0);
    }

    #[test]
    fn negative_scores_are_preserved_and_ranked_last() {
        // Opposed vectors are meaningfully *dis*similar; clamping them to 0
        // would lose that.
        let candidates = vec![
            candidate("opposed", vec![-1.0, 0.0]),
            candidate("orthogonal", vec![0.0, 1.0]),
        ];
        let r = rank(&[1.0, 0.0], &candidates, 10);
        assert_eq!(r.hits[0].sample_hash, "orthogonal");
        assert_eq!(r.hits[1].sample_hash, "opposed");
        assert!(r.hits[1].score < 0.0);
    }
}
