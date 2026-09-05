//! Matching a detected face against the player library (§6).
//!
//! The library is a bag of confirmed samples per player. A candidate is scored
//! against every sample and takes the *best* score for each player, rather than
//! being compared to a single average. That matters in practice: a player looks
//! different in a jersey under stage lighting than in a hoodie at a desk, and
//! averaging those together blurs both away.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::similarity::{centroid, cosine};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MatcherConfig {
    /// Similarity a face must reach before it is proposed as a known player.
    /// Exposed in Settings — §15 requires this to stay configurable.
    pub threshold: f32,
    /// How far ahead of the runner-up the winner must be. Guards against
    /// confidently mixing up two players who genuinely look alike.
    pub margin: f32,
    /// Two faces in the same photo cannot be the same person.
    pub unique_per_frame: bool,
}

impl Default for MatcherConfig {
    fn default() -> Self {
        Self {
            // Conservative defaults: a false positive is much more expensive
            // here than a missed suggestion because player albums feed the
            // sorting workflow. Editors can still lower these in Settings.
            threshold: 0.55,
            margin: 0.10,
            unique_per_frame: true,
        }
    }
}

/// One player's samples, indexed for matching.
#[derive(Debug, Clone)]
pub struct PersonProfile {
    pub person_id: i64,
    pub samples: Vec<Vec<f32>>,
    centroid: Vec<f32>,
}

impl PersonProfile {
    pub fn sample_count(&self) -> usize {
        self.samples.len()
    }

    /// The player's average appearance, useful as a tie-breaker and for
    /// comparing whole clusters against the library.
    pub fn centroid(&self) -> &[f32] {
        &self.centroid
    }

    fn best_similarity(&self, embedding: &[f32]) -> f32 {
        self.samples
            .iter()
            .map(|s| cosine(s, embedding))
            .fold(f32::NEG_INFINITY, f32::max)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Match {
    pub person_id: i64,
    pub similarity: f32,
    /// The best score any *other* player achieved, when there was one.
    pub runner_up: Option<f32>,
}

/// The reusable face library.
#[derive(Debug, Clone, Default)]
pub struct FaceMatcher {
    profiles: Vec<PersonProfile>,
}

impl FaceMatcher {
    /// Builds the library from `(person_id, embedding)` pairs — normally every
    /// confirmed face in the database.
    pub fn build(samples: impl IntoIterator<Item = (i64, Vec<f32>)>) -> Self {
        let mut grouped: HashMap<i64, Vec<Vec<f32>>> = HashMap::new();
        for (person_id, embedding) in samples {
            if embedding.is_empty() {
                continue;
            }
            grouped.entry(person_id).or_default().push(embedding);
        }

        let mut profiles: Vec<PersonProfile> = grouped
            .into_iter()
            .map(|(person_id, samples)| {
                let refs: Vec<&[f32]> = samples.iter().map(|s| s.as_slice()).collect();
                let centroid = centroid(&refs);
                PersonProfile {
                    person_id,
                    samples,
                    centroid,
                }
            })
            .collect();

        // Stable order keeps matching deterministic across runs.
        profiles.sort_by_key(|p| p.person_id);
        Self { profiles }
    }

    pub fn is_empty(&self) -> bool {
        self.profiles.is_empty()
    }

    pub fn player_count(&self) -> usize {
        self.profiles.len()
    }

    pub fn total_samples(&self) -> usize {
        self.profiles.iter().map(|p| p.samples.len()).sum()
    }

    pub fn profile(&self, person_id: i64) -> Option<&PersonProfile> {
        self.profiles.iter().find(|p| p.person_id == person_id)
    }

    /// Scores `embedding` against every player, best first.
    pub fn rank(&self, embedding: &[f32]) -> Vec<(i64, f32)> {
        let mut scores: Vec<(i64, f32)> = self
            .profiles
            .iter()
            .map(|p| (p.person_id, p.best_similarity(embedding)))
            .filter(|(_, s)| s.is_finite())
            .collect();
        scores.sort_by(|a, b| b.1.total_cmp(&a.1));
        scores
    }

    /// The single best match, or `None` when nothing clears the threshold and
    /// margin — in which case the face belongs in an unknown cluster (§6).
    pub fn match_one(&self, embedding: &[f32], config: &MatcherConfig) -> Option<Match> {
        let ranked = self.rank(embedding);
        let (person_id, similarity) = ranked.first().copied()?;
        if similarity < config.threshold {
            return None;
        }

        let runner_up = ranked.get(1).map(|(_, s)| *s);
        if let Some(second) = runner_up {
            if similarity - second < config.margin {
                return None;
            }
        }

        Some(Match {
            person_id,
            similarity,
            runner_up,
        })
    }

    /// Matches every face in one frame at once.
    ///
    /// With `unique_per_frame`, assignment is greedy by confidence: the
    /// strongest face/player pairing wins, and that player is then off the
    /// table for the other faces in the same image. A group photo cannot come
    /// back claiming the same player three times.
    pub fn match_frame(&self, embeddings: &[Vec<f32>], config: &MatcherConfig) -> Vec<Option<Match>> {
        if !config.unique_per_frame {
            return embeddings.iter().map(|e| self.match_one(e, config)).collect();
        }

        // A face may only claim its independently valid top identity. The old
        // global candidate list allowed a second face to fall through to its
        // second- or third-ranked player merely because its best player was
        // already present in the frame. That manufactured false identities in
        // group photos even though match_one() would have rejected them.
        let mut candidates: Vec<(usize, Match)> = embeddings
            .iter()
            .enumerate()
            .filter_map(|(face_index, embedding)| {
                self.match_one(embedding, config).map(|matched| (face_index, matched))
            })
            .collect();
        candidates.sort_by(|a, b| b.1.similarity.total_cmp(&a.1.similarity));

        let mut results: Vec<Option<Match>> = vec![None; embeddings.len()];
        let mut taken_people: Vec<i64> = Vec::new();

        for (face_index, matched) in candidates {
            if taken_people.contains(&matched.person_id) {
                continue;
            }
            taken_people.push(matched.person_id);
            results[face_index] = Some(matched);
        }

        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(v: Vec<f32>) -> Vec<f32> {
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.into_iter().map(|x| x / norm).collect()
    }

    /// Three well-separated identities in a 4-dimensional space.
    fn library() -> FaceMatcher {
        FaceMatcher::build(vec![
            (1, unit(vec![1.0, 0.0, 0.0, 0.0])), // Jonathan
            (1, unit(vec![0.95, 0.1, 0.0, 0.0])),
            (2, unit(vec![0.0, 1.0, 0.0, 0.0])), // Mavi
            (3, unit(vec![0.0, 0.0, 1.0, 0.0])), // Jelly
        ])
    }

    #[test]
    fn recognises_a_known_player() {
        let matcher = library();
        let probe = unit(vec![0.98, 0.05, 0.0, 0.0]);
        let result = matcher.match_one(&probe, &MatcherConfig::default()).unwrap();
        assert_eq!(result.person_id, 1);
        assert!(result.similarity > 0.9);
    }

    #[test]
    fn a_stranger_matches_nobody() {
        let matcher = library();
        let stranger = unit(vec![0.0, 0.0, 0.0, 1.0]);
        assert!(matcher.match_one(&stranger, &MatcherConfig::default()).is_none());
    }

    #[test]
    fn an_empty_library_matches_nothing() {
        let matcher = FaceMatcher::default();
        assert!(matcher.is_empty());
        assert!(matcher
            .match_one(&unit(vec![1.0, 0.0]), &MatcherConfig::default())
            .is_none());
    }

    #[test]
    fn the_margin_suppresses_ambiguous_matches() {
        // Two players whose samples sit almost on top of each other.
        let matcher = FaceMatcher::build(vec![(1, unit(vec![1.0, 0.0])), (2, unit(vec![0.999, 0.045]))]);
        let probe = unit(vec![1.0, 0.02]);

        let strict = MatcherConfig {
            threshold: 0.3,
            margin: 0.2,
            unique_per_frame: false,
        };
        assert!(
            matcher.match_one(&probe, &strict).is_none(),
            "ambiguity should be reported as unknown"
        );

        let permissive = MatcherConfig { margin: 0.0, ..strict };
        assert!(matcher.match_one(&probe, &permissive).is_some());
    }

    #[test]
    fn threshold_is_respected() {
        let matcher = library();
        let probe = unit(vec![0.7, 0.7, 0.0, 0.0]); // halfway between two players
        let low = MatcherConfig {
            threshold: 0.1,
            margin: 0.0,
            unique_per_frame: false,
        };
        let high = MatcherConfig {
            threshold: 0.95,
            margin: 0.0,
            unique_per_frame: false,
        };
        assert!(matcher.match_one(&probe, &low).is_some());
        assert!(matcher.match_one(&probe, &high).is_none());
    }

    #[test]
    fn one_frame_cannot_contain_the_same_player_twice() {
        let matcher = library();
        // Two faces that both look most like Jonathan; the weaker one must not
        // also be labelled Jonathan.
        let faces = vec![unit(vec![1.0, 0.0, 0.0, 0.0]), unit(vec![0.9, 0.15, 0.0, 0.0])];
        let config = MatcherConfig {
            threshold: 0.3,
            margin: 0.0,
            unique_per_frame: true,
        };

        let results = matcher.match_frame(&faces, &config);
        let assigned: Vec<i64> = results.iter().flatten().map(|m| m.person_id).collect();
        assert_eq!(
            assigned.len(),
            assigned.iter().collect::<std::collections::HashSet<_>>().len()
        );
        assert_eq!(results[0].unwrap().person_id, 1, "the strongest pairing wins");
    }

    #[test]
    fn distinct_players_in_one_frame_are_all_recognised() {
        let matcher = library();
        let faces = vec![
            unit(vec![1.0, 0.0, 0.0, 0.0]),
            unit(vec![0.0, 1.0, 0.0, 0.0]),
            unit(vec![0.0, 0.0, 1.0, 0.0]),
        ];
        let config = MatcherConfig {
            threshold: 0.4,
            margin: 0.05,
            unique_per_frame: true,
        };
        let results = matcher.match_frame(&faces, &config);

        assert_eq!(results[0].unwrap().person_id, 1);
        assert_eq!(results[1].unwrap().person_id, 2);
        assert_eq!(results[2].unwrap().person_id, 3);
    }

    #[test]
    fn a_taken_best_identity_does_not_force_a_face_onto_its_runner_up() {
        let matcher = FaceMatcher::build(vec![(1, unit(vec![1.0, 0.0])), (2, unit(vec![0.8, 0.6]))]);
        let faces = vec![unit(vec![1.0, 0.0]), unit(vec![0.98, 0.2])];
        let config = MatcherConfig {
            threshold: 0.55,
            margin: 0.05,
            unique_per_frame: true,
        };

        let results = matcher.match_frame(&faces, &config);
        assert_eq!(results[0].unwrap().person_id, 1);
        assert!(
            results[1].is_none(),
            "the second face is not independently a match for player 2"
        );
    }

    #[test]
    fn adding_samples_improves_coverage() {
        // A correction adds a sample; the previously unmatched pose now matches.
        let awkward_pose = unit(vec![0.6, 0.0, 0.0, 0.8]);
        let config = MatcherConfig {
            threshold: 0.7,
            margin: 0.0,
            unique_per_frame: false,
        };

        let before = library();
        assert!(before.match_one(&awkward_pose, &config).is_none());

        let after = FaceMatcher::build(vec![
            (1, unit(vec![1.0, 0.0, 0.0, 0.0])),
            (1, awkward_pose.clone()), // the human correction
            (2, unit(vec![0.0, 1.0, 0.0, 0.0])),
        ]);
        assert_eq!(after.match_one(&awkward_pose, &config).unwrap().person_id, 1);
    }

    #[test]
    fn library_statistics_are_reported() {
        let matcher = library();
        assert_eq!(matcher.player_count(), 3);
        assert_eq!(matcher.total_samples(), 4);
        assert_eq!(matcher.profile(1).unwrap().sample_count(), 2);
        assert!(matcher.profile(99).is_none());
    }
}
