//! Grouping unknown faces (§7).
//!
//! The number of strangers in a shoot is not known in advance, which rules out
//! anything that needs `k` up front. Chinese Whispers fits: build a graph where
//! each face links to its most similar peers, give everyone their own label,
//! then repeatedly let each node adopt whichever label its neighbours support
//! most strongly. Dense regions converge on a shared label; sparse ones stay
//! alone.

use serde::{Deserialize, Serialize};

use crate::similarity::{centroid, cosine, knn_graph, Neighbour};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClusterConfig {
    /// Two faces are only linked if they are at least this similar. The single
    /// most important knob: too low merges different players, too high splits
    /// one player across several clusters.
    pub edge_threshold: f32,
    /// How many neighbours each face may link to.
    pub neighbours: usize,
    /// Label-propagation rounds. Convergence is normally well inside this.
    pub iterations: usize,
    /// Groups smaller than this are noise, not a person worth reviewing.
    pub min_cluster_size: usize,
    /// After propagation, clusters whose centroids are at least this similar
    /// are folded together — the pass that stops one player appearing as
    /// "Unknown Person 2" and "Unknown Person 5".
    pub merge_threshold: f32,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            edge_threshold: 0.45,
            neighbours: 12,
            iterations: 24,
            min_cluster_size: 3,
            merge_threshold: 0.62,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Cluster {
    /// Indices into the slice that was clustered.
    pub members: Vec<usize>,
    pub centroid: Vec<f32>,
    /// Mean similarity of members to the centroid: how tight the group is.
    pub cohesion: f32,
}

impl Cluster {
    pub fn size(&self) -> usize {
        self.members.len()
    }
}

#[derive(Debug, Clone, Default)]
pub struct ClusterResult {
    /// Largest group first, which is the order the review screen wants.
    pub clusters: Vec<Cluster>,
    /// Faces too isolated to belong anywhere.
    pub unclustered: Vec<usize>,
}

impl ClusterResult {
    pub fn cluster_count(&self) -> usize {
        self.clusters.len()
    }

    /// Which cluster each input index landed in, or `None` for noise.
    pub fn assignment(&self, total: usize) -> Vec<Option<usize>> {
        let mut out = vec![None; total];
        for (cluster_index, cluster) in self.clusters.iter().enumerate() {
            for member in &cluster.members {
                if *member < total {
                    out[*member] = Some(cluster_index);
                }
            }
        }
        out
    }
}

/// A tiny deterministic PRNG.
///
/// Chinese Whispers needs to visit nodes in a shuffled order or it develops a
/// bias toward low indices, but re-running an import must give the same answer
/// every time — so the randomness is seeded and reproducible rather than drawn
/// from the system.
struct Shuffler(u64);

impl Shuffler {
    fn new(seed: u64) -> Self {
        Shuffler(seed | 1)
    }

    fn next(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn shuffle(&mut self, items: &mut [usize]) {
        for i in (1..items.len()).rev() {
            let j = (self.next() % (i as u64 + 1)) as usize;
            items.swap(i, j);
        }
    }
}

/// Groups `embeddings` into clusters of the same person.
pub fn cluster_faces(embeddings: &[Vec<f32>], config: &ClusterConfig) -> ClusterResult {
    let n = embeddings.len();
    if n == 0 {
        return ClusterResult::default();
    }
    if n == 1 {
        return ClusterResult { clusters: Vec::new(), unclustered: vec![0] };
    }

    let graph = knn_graph(embeddings, config.neighbours.max(1), config.edge_threshold);
    let labels = propagate(&graph, config.iterations);
    let groups = collect_groups(&labels, n);

    let all: Vec<Cluster> = groups
        .into_iter()
        .map(|members| build_cluster(embeddings, members))
        .collect();

    // Only groups the graph actually linked take part in the merge pass. A
    // singleton is a face that failed to reach *anyone* at `edge_threshold`;
    // folding it in on the strength of a looser centroid comparison would
    // contradict the decision the graph just made.
    let (linked, singletons): (Vec<Cluster>, Vec<Cluster>) = all.into_iter().partition(|c| c.size() > 1);
    let mut clusters = merge_close_clusters(embeddings, linked, config.merge_threshold);
    clusters.extend(singletons);

    // Split off the groups too small to be worth a reviewer's time.
    let mut unclustered: Vec<usize> = Vec::new();
    clusters.retain(|c| {
        if c.size() < config.min_cluster_size.max(1) {
            unclustered.extend(c.members.iter().copied());
            false
        } else {
            true
        }
    });

    clusters.sort_by(|a, b| b.size().cmp(&a.size()).then(a.members[0].cmp(&b.members[0])));
    unclustered.sort_unstable();

    ClusterResult { clusters, unclustered }
}

/// Label propagation: each node repeatedly adopts the label its neighbours
/// back most strongly, weighted by similarity.
fn propagate(graph: &[Vec<Neighbour>], iterations: usize) -> Vec<usize> {
    let n = graph.len();
    let mut labels: Vec<usize> = (0..n).collect();
    let mut order: Vec<usize> = (0..n).collect();
    let mut shuffler = Shuffler::new(0x5EED_1234_ABCD_0001);

    for _ in 0..iterations.max(1) {
        shuffler.shuffle(&mut order);
        let mut changed = false;

        for &node in &order {
            if graph[node].is_empty() {
                continue;
            }

            // Sum edge weights per candidate label.
            let mut best_label = labels[node];
            let mut best_weight = 0.0f32;
            let mut tallies: Vec<(usize, f32)> = Vec::with_capacity(graph[node].len());

            for neighbour in &graph[node] {
                let label = labels[neighbour.index];
                match tallies.iter_mut().find(|(l, _)| *l == label) {
                    Some(entry) => entry.1 += neighbour.similarity,
                    None => tallies.push((label, neighbour.similarity)),
                }
            }
            for (label, weight) in tallies {
                // Ties break toward the lower label so the result is stable.
                if weight > best_weight || (weight == best_weight && label < best_label) {
                    best_weight = weight;
                    best_label = label;
                }
            }

            if best_label != labels[node] {
                labels[node] = best_label;
                changed = true;
            }
        }

        if !changed {
            break; // converged
        }
    }

    labels
}

fn collect_groups(labels: &[usize], n: usize) -> Vec<Vec<usize>> {
    let mut by_label: Vec<(usize, Vec<usize>)> = Vec::new();
    for (index, &label) in labels.iter().enumerate().take(n) {
        match by_label.iter_mut().find(|(l, _)| *l == label) {
            Some(entry) => entry.1.push(index),
            None => by_label.push((label, vec![index])),
        }
    }
    by_label.into_iter().map(|(_, members)| members).collect()
}

fn build_cluster(embeddings: &[Vec<f32>], members: Vec<usize>) -> Cluster {
    let refs: Vec<&[f32]> = members.iter().map(|i| embeddings[*i].as_slice()).collect();
    let centre = centroid(&refs);
    let cohesion = if members.is_empty() {
        0.0
    } else {
        refs.iter().map(|v| cosine(v, &centre)).sum::<f32>() / members.len() as f32
    };
    Cluster { members, centroid: centre, cohesion }
}

/// Folds together clusters whose centroids are close enough to be the same
/// person seen under different conditions.
fn merge_close_clusters(embeddings: &[Vec<f32>], clusters: Vec<Cluster>, threshold: f32) -> Vec<Cluster> {
    if clusters.len() < 2 || threshold >= 1.0 {
        return clusters;
    }

    let mut merged: Vec<Cluster> = Vec::with_capacity(clusters.len());
    for cluster in clusters {
        let target = merged
            .iter()
            .position(|existing| cosine(&existing.centroid, &cluster.centroid) >= threshold);

        match target {
            Some(index) => {
                let mut members = std::mem::take(&mut merged[index].members);
                members.extend(cluster.members);
                members.sort_unstable();
                members.dedup();
                merged[index] = build_cluster(embeddings, members);
            }
            None => merged.push(cluster),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(v: Vec<f32>) -> Vec<f32> {
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.into_iter().map(|x| x / norm).collect()
    }

    /// Builds `count` slightly different views of one identity.
    fn identity(axis: usize, dim: usize, count: usize, jitter: f32) -> Vec<Vec<f32>> {
        (0..count)
            .map(|i| {
                let mut v = vec![0.0f32; dim];
                v[axis] = 1.0;
                // Nudge along a neighbouring axis so samples are not identical.
                v[(axis + 1) % dim] = jitter * (i as f32 / count.max(1) as f32);
                unit(v)
            })
            .collect()
    }

    #[test]
    fn separates_three_people() {
        let mut embeddings = Vec::new();
        embeddings.extend(identity(0, 6, 8, 0.15));
        embeddings.extend(identity(2, 6, 6, 0.15));
        embeddings.extend(identity(4, 6, 5, 0.15));

        let result = cluster_faces(&embeddings, &ClusterConfig::default());
        assert_eq!(result.cluster_count(), 3, "expected one cluster per identity");

        let sizes: Vec<usize> = result.clusters.iter().map(|c| c.size()).collect();
        assert_eq!(sizes, vec![8, 6, 5], "clusters come back largest first");

        // Nobody should be mixed: every member of a cluster shares an identity.
        let assignment = result.assignment(embeddings.len());
        assert_eq!(assignment[0], assignment[7]);
        assert_ne!(assignment[0], assignment[8]);
    }

    #[test]
    fn is_deterministic_across_runs() {
        let mut embeddings = Vec::new();
        embeddings.extend(identity(0, 8, 7, 0.2));
        embeddings.extend(identity(3, 8, 7, 0.2));

        let first = cluster_faces(&embeddings, &ClusterConfig::default());
        let second = cluster_faces(&embeddings, &ClusterConfig::default());
        assert_eq!(first.clusters, second.clusters);
    }

    #[test]
    fn isolated_faces_are_left_unclustered() {
        let mut embeddings = identity(0, 6, 6, 0.1);
        embeddings.push(unit(vec![0.0, 0.0, 0.0, 0.0, 0.0, 1.0])); // one stranger

        let result = cluster_faces(&embeddings, &ClusterConfig::default());
        assert_eq!(result.cluster_count(), 1);
        assert_eq!(result.unclustered, vec![6]);
    }

    #[test]
    fn min_cluster_size_filters_small_groups() {
        let mut embeddings = identity(0, 6, 6, 0.1);
        embeddings.extend(identity(3, 6, 2, 0.1)); // a pair

        let strict = ClusterConfig { min_cluster_size: 4, ..Default::default() };
        let result = cluster_faces(&embeddings, &strict);
        assert_eq!(result.cluster_count(), 1);
        assert_eq!(result.unclustered.len(), 2);

        let lenient = ClusterConfig { min_cluster_size: 2, ..Default::default() };
        assert_eq!(cluster_faces(&embeddings, &lenient).cluster_count(), 2);
    }

    #[test]
    fn a_high_edge_threshold_leaves_everything_unclustered() {
        let embeddings = identity(0, 6, 6, 0.3);
        let config = ClusterConfig { edge_threshold: 0.999, ..Default::default() };
        let result = cluster_faces(&embeddings, &config);
        assert_eq!(result.cluster_count(), 0);
        assert_eq!(result.unclustered.len(), 6);
    }

    #[test]
    fn cohesion_is_higher_for_a_tight_group() {
        let tight = cluster_faces(&identity(0, 6, 6, 0.02), &ClusterConfig::default());
        let loose = cluster_faces(&identity(0, 6, 6, 0.5), &ClusterConfig::default());
        assert!(tight.clusters[0].cohesion >= loose.clusters[0].cohesion);
        assert!(tight.clusters[0].cohesion <= 1.0001);
    }

    #[test]
    fn empty_and_single_inputs_are_handled() {
        assert_eq!(cluster_faces(&[], &ClusterConfig::default()).cluster_count(), 0);
        let single = cluster_faces(&[unit(vec![1.0, 0.0])], &ClusterConfig::default());
        assert_eq!(single.cluster_count(), 0);
        assert_eq!(single.unclustered, vec![0]);
    }

    #[test]
    fn shuffler_permutes_without_losing_elements() {
        let mut items: Vec<usize> = (0..50).collect();
        Shuffler::new(7).shuffle(&mut items);
        assert_eq!(items.len(), 50);
        items.sort_unstable();
        assert_eq!(items, (0..50).collect::<Vec<_>>());
    }

    #[test]
    fn assignment_maps_every_member_back() {
        let embeddings = identity(0, 6, 5, 0.1);
        let result = cluster_faces(&embeddings, &ClusterConfig::default());
        let assignment = result.assignment(embeddings.len());
        assert!(assignment.iter().all(|a| a.is_some()));
    }
}
