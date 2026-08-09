//! Turning embeddings into people.
//!
//! Two halves, matching the two branches of §6: a [`FaceMatcher`] that compares
//! a face against players the application already knows, and a clusterer that
//! groups whatever is left over so a human can name each group once.

pub mod cluster;
pub mod matcher;
pub mod similarity;

pub use cluster::{cluster_faces, Cluster, ClusterConfig, ClusterResult};
pub use matcher::{FaceMatcher, Match, MatcherConfig, PersonProfile};
pub use similarity::{centroid, cosine, knn_graph, Neighbour};

/// Suggests which existing player an unnamed cluster probably is.
///
/// Used to pre-fill the rename box in the review screen: the cluster's average
/// face is compared against the library, and anything convincing is offered as
/// a suggestion the reviewer can accept or ignore.
pub fn suggest_person_for_cluster(
    cluster: &Cluster,
    matcher: &FaceMatcher,
    config: &MatcherConfig,
) -> Option<Match> {
    if cluster.centroid.is_empty() {
        return None;
    }
    matcher.match_one(&cluster.centroid, config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(v: Vec<f32>) -> Vec<f32> {
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.into_iter().map(|x| x / norm).collect()
    }

    #[test]
    fn a_cluster_of_a_known_player_is_suggested() {
        let matcher = FaceMatcher::build(vec![
            (7, unit(vec![1.0, 0.0, 0.0])),
            (8, unit(vec![0.0, 1.0, 0.0])),
        ]);
        let embeddings: Vec<Vec<f32>> = (0..5)
            .map(|i| unit(vec![1.0, 0.02 * i as f32, 0.0]))
            .collect();

        let result = cluster_faces(&embeddings, &ClusterConfig::default());
        let suggestion = suggest_person_for_cluster(&result.clusters[0], &matcher, &MatcherConfig::default());
        assert_eq!(suggestion.unwrap().person_id, 7);
    }

    #[test]
    fn a_cluster_of_a_stranger_gets_no_suggestion() {
        let matcher = FaceMatcher::build(vec![(7, unit(vec![1.0, 0.0, 0.0]))]);
        let embeddings: Vec<Vec<f32>> = (0..5)
            .map(|i| unit(vec![0.0, 0.02 * i as f32, 1.0]))
            .collect();

        let result = cluster_faces(&embeddings, &ClusterConfig::default());
        assert!(suggest_person_for_cluster(&result.clusters[0], &matcher, &MatcherConfig::default()).is_none());
    }
}
