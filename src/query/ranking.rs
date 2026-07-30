use std::collections::HashMap;

use super::ScoreBreakdown;

const RRF_K: f32 = 60.0;

#[derive(Clone, Debug)]
pub(crate) struct ScoredId {
    pub chunk_id: String,
    pub score: f32,
}

#[derive(Clone, Debug)]
pub(crate) struct RankedId {
    pub chunk_id: String,
    pub scores: ScoreBreakdown,
    pub final_score: f32,
}

pub(crate) fn fuse(
    exact: &[ScoredId],
    lexical: &[ScoredId],
    semantic: &[ScoredId],
    boost: impl Fn(&str) -> f32,
) -> Vec<RankedId> {
    let mut scores = HashMap::<String, ScoreBreakdown>::new();
    for (index, candidate) in exact.iter().enumerate() {
        let score = scores.entry(candidate.chunk_id.clone()).or_default();
        score.exact_rank = Some((index + 1) as u32);
        score.rrf_score += reciprocal_rank(index);
    }
    for (index, candidate) in lexical.iter().enumerate() {
        let score = scores.entry(candidate.chunk_id.clone()).or_default();
        score.lexical_rank = Some((index + 1) as u32);
        score.lexical_score = Some(candidate.score);
        score.rrf_score += reciprocal_rank(index);
    }
    for (index, candidate) in semantic.iter().enumerate() {
        let score = scores.entry(candidate.chunk_id.clone()).or_default();
        score.semantic_rank = Some((index + 1) as u32);
        score.semantic_score = Some(candidate.score);
        score.rrf_score += reciprocal_rank(index);
    }
    let mut ranked = scores
        .into_iter()
        .map(|(chunk_id, mut scores)| {
            scores.boost = boost(&chunk_id);
            RankedId {
                final_score: scores.rrf_score * scores.boost,
                chunk_id,
                scores,
            }
        })
        .collect::<Vec<_>>();
    ranked.sort_unstable_by(|left, right| {
        right
            .final_score
            .total_cmp(&left.final_score)
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    ranked
}

fn reciprocal_rank(index: usize) -> f32 {
    1.0 / (RRF_K + (index + 1) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scored(id: &str, score: f32) -> ScoredId {
        ScoredId {
            chunk_id: id.into(),
            score,
        }
    }

    #[test]
    fn rrf_fuses_lists_and_breaks_ties_by_chunk_id() {
        let ranked = fuse(
            &[scored("exact", 1.0)],
            &[scored("shared", 9.0), scored("exact", 8.0)],
            &[scored("shared", 0.9), scored("semantic", 0.8)],
            |_| 1.0,
        );
        assert_eq!(ranked[0].chunk_id, "shared");
        assert!(ranked[0].scores.lexical_rank.is_some());
        assert!(ranked[0].scores.semantic_rank.is_some());
        assert_eq!(ranked[1].chunk_id, "exact");
    }

    #[test]
    fn deterministic_boost_changes_final_order() {
        let ranked = fuse(&[], &[scored("a", 2.0), scored("b", 1.0)], &[], |id| {
            if id == "b" { 2.0 } else { 1.0 }
        });
        assert_eq!(ranked[0].chunk_id, "b");
        assert_eq!(ranked[0].scores.boost, 2.0);
    }
}
